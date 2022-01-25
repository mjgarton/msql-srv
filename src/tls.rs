use std::io::{self, Cursor};
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
    Plain(T),
    Tls(rustls::StreamOwned<ServerConnection, PrependedReader<T>>),
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
        SwitchableConn(Some(EitherConn::Plain(rw)))
    }

    pub fn switch_to_tls(&mut self, config: Arc<ServerConfig>, extra: &[u8]) -> io::Result<()> {
        let replacement = match self.0.take() {
            Some(EitherConn::Plain(plain)) => {
                let plain = PrependedReader::new(plain, extra);
                Ok(EitherConn::Tls(create_stream(plain, config)?))
            }
            Some(EitherConn::Tls(_)) => Err(io::Error::new(
                io::ErrorKind::Other,
                "tls variant found when plain was expected",
            )),
            None => unreachable!(),
        }?;

        self.0 = Some(replacement);
        Ok(())
    }
}

pub(crate) struct PrependedReader<RW: Read + Write> {
    prepended: Option<Cursor<Vec<u8>>>,
    inner: RW,
}

impl<RW: Read + Write> PrependedReader<RW> {
    fn new(rw: RW, prepended: &[u8]) -> PrependedReader<RW> {
        PrependedReader {
            inner: rw,
            prepended: if prepended.is_empty() {
                None
            } else {
                Some(Cursor::new(prepended.to_vec()))
            },
        }
    }
}

impl<RW: Read + Write> Read for PrependedReader<RW> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.prepended {
            Some(r) => {
                let i = r.read(buf)?;
                if i != 0 {
                    Ok(i)
                } else {
                    self.prepended = None;
                    self.inner.read(buf)
                }
            }
            None => self.inner.read(buf),
        }
    }
}

impl<RW: Read + Write> Write for PrependedReader<RW> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
