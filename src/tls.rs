use std::collections::VecDeque;
use std::io::{self, BufRead, Cursor};
use std::io::{Read, Write};
use std::sync::Arc;

use rustls::{self, ServerConfig, ServerConnection};

pub fn create_stream<T: Read + Write + Sized>(
    sock: T,
    config: Arc<ServerConfig>,
) -> Result<rustls::StreamOwned<ServerConnection, T>, io::Error> {
    let conn = ServerConnection::new(config).unwrap();
    let stream = rustls::StreamOwned { conn, sock };
    Ok(stream)
}

pub(crate) struct SwitchableConn<T: Read + Write>(Option<EitherConn<T>>);

pub(crate) enum EitherConn<T: Read + Write> {
    Plain(BufReader<T>),
    Tls(rustls::StreamOwned<ServerConnection, BufReader<T>>),
}

impl<T: Read + Write> Read for SwitchableConn<T> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.0.as_mut().unwrap() {
            EitherConn::Plain(p) => p.read(buf),
            EitherConn::Tls(t) => t.read(buf),
        }
    }
}

impl<T: Read + Write> Write for SwitchableConn<T> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.0.as_mut().unwrap() {
            EitherConn::Plain(p) => p.write(buf),
            EitherConn::Tls(t) => t.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.0.as_mut().unwrap() {
            EitherConn::Plain(p) => p.flush(),
            EitherConn::Tls(t) => t.flush(),
        }
    }
}

impl<T: Read + Write> SwitchableConn<T> {
    pub fn new(rw: T) -> SwitchableConn<T> {
        SwitchableConn(Some(EitherConn::Plain(BufReader::new(rw))))
    }

    pub fn switch_to_tls(&mut self, config: Arc<ServerConfig>) -> io::Result<()> {
        let replacement = match self.0.take() {
            Some(EitherConn::Plain(plain)) => Ok(EitherConn::Tls(create_stream(plain, config)?)),
            Some(EitherConn::Tls(_)) => Err(io::Error::new(
                io::ErrorKind::Other,
                "tls variant found when plain was expected",
            )),
            None => unreachable!(),
        }?;

        self.0 = Some(replacement);
        Ok(())
    }

    pub(crate) fn replace(&mut self, bytes: &[u8]) {
        match &mut self.0 {
            Some(EitherConn::Plain(p)) => p.replace(bytes),
            _ => panic!("can not replace raw bytes."),
        }
    }
}

pub(crate) struct BufReader<RW: Read + Write> {
    replaced: Option<Cursor<Vec<u8>>>,
    inner: std::io::BufReader<RW>,
}

impl<RW: Read + Write> BufReader<RW> {
    fn new(rw: RW) -> BufReader<RW> {
        BufReader {
            inner: std::io::BufReader::new(rw),
            replaced: None,
        }
    }

    fn replace(&mut self, data: &[u8]) {
        match &mut self.replaced {
            Some(r) => {
                let v = r.get_mut();
                let mut v_new = Vec::from(data);
                v_new.extend_from_slice(v);
                *v = v_new;
            }
            None => self.replaced = Some(Cursor::new(data.to_vec())),
        }
    }
}

impl<RW: Read + Write> Read for BufReader<RW> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.replaced {
            Some(r) => {
                let i = r.read(buf)?;
                if i != 0 {
                    Ok(i)
                } else {
                    self.replaced = None;
                    self.inner.read(buf)
                }
            }
            None => self.inner.read(buf),
        }
    }
}

impl<RW: Read + Write> Write for BufReader<RW> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.get_mut().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.get_mut().flush()
    }
}

impl<RW: Read + Write> BufRead for BufReader<RW> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        self.inner.fill_buf()
    }

    fn consume(&mut self, amt: usize) {
        self.inner.consume(amt)
    }
}

// #[cfg(test)]
// mod tests {
//     use std::{
//         io::{Cursor, Read, Write},
//         sync::Arc,
//     };

//     use rustls::{Certificate, PrivateKey, ServerConfig};

//     use crate::{MysqlIntermediary, MysqlShim};

//     #[test]
//     fn test_run_on_with_tls_with_extra_data() {
//         // This data represents a client handshake, including a request to switch to TLS mode,
//         // but _also_ provides the later (tls encrypted) data at the same time.  This is intended
//         // to ensure the switch to TLS happens at the right point in the stream, and we don't end
//         // up reading part of the encrypted data into the buffer before doing the switch to TLS.
//         let data = vec![
//             0x20, 0x00, 0x00, 0x01, 0x81, 0xaa, 0x1f, 0x00, 0x00, 0x00, 0x10, 0x00, 0x21, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x16, 0x03, 0x01, 0x02, 0x00, 0x01,
//             0x00, 0x01, 0xfc, 0x03, 0x03, 0x52, 0x58, 0xce, 0x72, 0xda, 0x0c, 0x93, 0x93, 0x8b,
//             0x73, 0x90, 0x2b, 0x85, 0x65, 0xd3, 0x3c, 0x74, 0x7c, 0xa2, 0xbd, 0xcb, 0x15, 0xfb,
//             0x5e, 0x7f, 0x22, 0x8f, 0x29, 0x5e, 0x5a, 0x74, 0xe7, 0x20, 0x9b, 0x4d, 0xe3, 0x6a,
//             0xc8, 0x79, 0x50, 0xa6, 0x76, 0xdb, 0xc4, 0x6a, 0x61, 0xac, 0x6c, 0xbd, 0x0a, 0xb8,
//             0x75, 0xeb, 0xa7, 0xd9, 0x51, 0xde, 0xb8, 0x74, 0x29, 0xfd, 0xa6, 0x97, 0x3f, 0xa4,
//             0x00, 0x40, 0x13, 0x02, 0x13, 0x03, 0x13, 0x01, 0x13, 0x04, 0xc0, 0x2c, 0xc0, 0x30,
//             0x00, 0x9f, 0xcc, 0xa9, 0xcc, 0xa8, 0xcc, 0xaa, 0xc0, 0x2b, 0xc0, 0x2f, 0x00, 0x9e,
//             0xc0, 0x24, 0xc0, 0x28, 0x00, 0x6b, 0xc0, 0x23, 0xc0, 0x27, 0x00, 0x67, 0xc0, 0x0a,
//             0xc0, 0x14, 0x00, 0x39, 0xc0, 0x09, 0xc0, 0x13, 0x00, 0x33, 0x00, 0x9d, 0x00, 0x9c,
//             0x00, 0x3d, 0x00, 0x3c, 0x00, 0x35, 0x00, 0x2f, 0x00, 0xff, 0x01, 0x00, 0x01, 0x73,
//             0x00, 0x00, 0x00, 0x0e, 0x00, 0x0c, 0x00, 0x00, 0x09, 0x6c, 0x6f, 0x63, 0x61, 0x6c,
//             0x68, 0x6f, 0x73, 0x74, 0x00, 0x0b, 0x00, 0x04, 0x03, 0x00, 0x01, 0x02, 0x00, 0x0a,
//             0x00, 0x0c, 0x00, 0x0a, 0x00, 0x1d, 0x00, 0x17, 0x00, 0x1e, 0x00, 0x19, 0x00, 0x18,
//             0x00, 0x23, 0x00, 0x00, 0x00, 0x16, 0x00, 0x00, 0x00, 0x17, 0x00, 0x00, 0x00, 0x0d,
//             0x00, 0x30, 0x00, 0x2e, 0x04, 0x03, 0x05, 0x03, 0x06, 0x03, 0x08, 0x07, 0x08, 0x08,
//             0x08, 0x09, 0x08, 0x0a, 0x08, 0x0b, 0x08, 0x04, 0x08, 0x05, 0x08, 0x06, 0x04, 0x01,
//             0x05, 0x01, 0x06, 0x01, 0x03, 0x03, 0x03, 0x01, 0x04, 0x02, 0x05, 0x02, 0x06, 0x02,
//             0x03, 0x02, 0x02, 0x03, 0x02, 0x01, 0x02, 0x02, 0x00, 0x2b, 0x00, 0x09, 0x08, 0x03,
//             0x04, 0x03, 0x03, 0x03, 0x02, 0x03, 0x01, 0x00, 0x2d, 0x00, 0x02, 0x01, 0x01, 0x00,
//             0x33, 0x00, 0x26, 0x00, 0x24, 0x00, 0x1d, 0x00, 0x20, 0x7e, 0x70, 0xf6, 0x4e, 0x87,
//             0x31, 0x29, 0xe6, 0x47, 0x0f, 0x75, 0x30, 0x83, 0x31, 0x9b, 0x4d, 0x87, 0x74, 0xf1,
//             0x71, 0x5a, 0x04, 0x61, 0x63, 0xef, 0x0b, 0x7f, 0x29, 0x12, 0x66, 0x93, 0x11, 0x00,
//             0x15, 0x00, 0xc8, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
//             0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x14, 0x03, 0x03, 0x00, 0x01, 0x01, 0x17,
//             0x03, 0x03, 0x00, 0x45, 0xc1, 0x19, 0x7b, 0xbc, 0xe8, 0xb4, 0x12, 0xc7, 0xa2, 0x40,
//             0x6f, 0x30, 0x71, 0x03, 0x62, 0xa2, 0xc1, 0x45, 0xb9, 0xc8, 0xba, 0x70, 0x3f, 0x22,
//             0xc3, 0x30, 0xef, 0xd9, 0x53, 0xe6, 0x39, 0x5d, 0x8f, 0x3f, 0x04, 0x23, 0xb9, 0x1e,
//             0xf7, 0xc4, 0x5f, 0xff, 0xc6, 0xc1, 0x16, 0x89, 0xf4, 0xb2, 0xf8, 0x93, 0x02, 0x4a,
//             0x61, 0x01, 0xb2, 0x0a, 0x8b, 0x69, 0x2d, 0xab, 0xcf, 0xbd, 0xb5, 0x81, 0x45, 0x73,
//             0x0d, 0x8f, 0xd1, 0x17, 0x03, 0x03, 0x00, 0x38, 0x27, 0x32, 0xa8, 0x92, 0x7a, 0xf7,
//             0xc2, 0xc3, 0x36, 0xe9, 0x26, 0x3d, 0x11, 0x86, 0xdf, 0xb8, 0xbd, 0x1f, 0x7d, 0x95,
//             0x7a, 0x84, 0x9a, 0x2d, 0x81, 0x8d, 0xcc, 0x55, 0xc3, 0x1d, 0xe8, 0xaa, 0x6c, 0x72,
//             0xab, 0xd6, 0x05, 0x84, 0xde, 0x52, 0x9e, 0x32, 0xd3, 0xef, 0x44, 0x29, 0xb3, 0x50,
//             0x72, 0x11, 0x87, 0xae, 0x14, 0xf5, 0xd8, 0xb8,
//         ];

//         struct Rw {
//             data: Cursor<Vec<u8>>,
//         }
//         impl Read for Rw {
//             fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
//                 self.data.read(buf)
//             }
//         }
//         impl Write for Rw {
//             fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
//                 Ok(buf.len())
//             }
//             fn flush(&mut self) -> std::io::Result<()> {
//                 Ok(())
//             }
//         }

//         let rw = Rw {
//             data: Cursor::new(data.to_vec()),
//         };

//         struct Shim {
//             cert: rcgen::Certificate,
//         }

//         impl<RW: Read + Write> MysqlShim<RW> for Shim {
//             type Error = std::io::Error;

//             fn on_prepare(
//                 &mut self,
//                 _: &str,
//                 _: crate::StatementMetaWriter<'_, RW>,
//             ) -> Result<(), Self::Error> {
//                 unreachable!()
//             }

//             fn on_execute(
//                 &mut self,
//                 _: u32,
//                 _: crate::ParamParser<'_>,
//                 _: crate::QueryResultWriter<'_, RW>,
//             ) -> Result<(), Self::Error> {
//                 unreachable!()
//             }

//             fn on_close(&mut self, _stmt: u32) {
//                 unreachable!()
//             }

//             fn on_query(
//                 &mut self,
//                 _: &str,
//                 _: crate::QueryResultWriter<'_, RW>,
//             ) -> Result<(), Self::Error> {
//                 unreachable!()
//             }

//             fn tls_config(&self) -> Option<std::sync::Arc<rustls::ServerConfig>> {
//                 let sc = ServerConfig::builder()
//                     .with_safe_defaults()
//                     .with_no_client_auth()
//                     .with_single_cert(
//                         vec![Certificate(self.cert.serialize_der().unwrap())],
//                         PrivateKey(self.cert.get_key_pair().serialize_der()),
//                     )
//                     .unwrap();

//                 Some(Arc::new(sc))
//             }
//         }

//         let shim = Shim {
//             cert: rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap(),
//         };

//         MysqlIntermediary::run_on(shim, rw).unwrap()
//     }
// }
