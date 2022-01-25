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
