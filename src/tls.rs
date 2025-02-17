use std::io::{BufReader, Read, Write};
use std::sync::{Arc, Mutex};
use std::{fs, io};

use rustls::{
    self, AllowAnyAuthenticatedClient, Certificate, Connection, NoClientAuth, RootCertStore,
    ServerConfig, ServerConnection,
};

/// TLS configuration
#[derive(Clone)]
pub struct TlsConfig {
    /// Full path to the server certificate file.
    pub server_cert: String,
    /// Full path to the server key file.
    pub server_cert_key: String,
    /// Optional full path to a folder of client certificates to validate against.
    pub client_cert_dir: Option<String>,
    /// Require clients to use TLS?
    pub require_tls: bool,
}

fn make_config(config: &TlsConfig) -> Result<Arc<rustls::ServerConfig>, io::Error> {
    let client_auth = if config.client_cert_dir.is_some() {
        let mut client_auth_roots = RootCertStore::empty();

        let paths = fs::read_dir(config.client_cert_dir.as_ref().unwrap()).unwrap();

        for path in paths {
            let path = path.unwrap();
            if !path.path().is_file() {
                continue;
            }

            let roots = load_cert(path.path().to_str().unwrap())?;

            for root in roots {
                client_auth_roots.add(&root).unwrap();
            }
        }

        AllowAnyAuthenticatedClient::new(client_auth_roots)
    } else {
        NoClientAuth::new()
    };

    let builder = ServerConfig::builder()
        .with_safe_default_cipher_suites()
        .with_safe_default_kx_groups()
        .with_safe_default_protocol_versions()
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::Other,
                "inconsistent cipher-suites/versions specified",
            )
        })?
        .with_client_cert_verifier(client_auth);

    let mut server_config = {
        let certs = load_cert(&config.server_cert)?;
        let privkey = load_private_key(&config.server_cert_key)?;

        builder
            .with_single_cert(certs, privkey)
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "bad certificates/private key"))?
    };

    server_config.key_log = Arc::new(rustls::KeyLogFile::new());

    Ok(Arc::new(server_config))
}

fn load_cert(filename: &str) -> Result<Vec<rustls::Certificate>, io::Error> {
    let certfile = fs::File::open(filename)?;
    let mut reader = BufReader::new(certfile);
    Ok(rustls_pemfile::certs(&mut reader)?
        .iter()
        .map(|v| rustls::Certificate(v.clone()))
        .collect())
}

fn load_private_key(filename: &str) -> Result<rustls::PrivateKey, io::Error> {
    let keyfile = fs::File::open(filename)?;
    let mut reader = BufReader::new(keyfile);

    loop {
        match rustls_pemfile::read_one(&mut reader).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "cannot parse private key .pem file",
            )
        })? {
            Some(rustls_pemfile::Item::RSAKey(key)) => return Ok(rustls::PrivateKey(key)),
            Some(rustls_pemfile::Item::PKCS8Key(key)) => return Ok(rustls::PrivateKey(key)),
            None => break,
            _ => {}
        }
    }

    Err(io::Error::new(
        io::ErrorKind::Other,
        format!(
            "no keys found in {:?} (encrypted keys not supported)",
            filename
        ),
    ))
}

pub struct TlsStream<C: Connection + Sized, T: Read + Write + Sized> {
    stream: Arc<Mutex<rustls::StreamOwned<C, T>>>,
}

impl<C, T> Clone for TlsStream<C, T>
where
    C: Connection,
    T: Read + Write,
{
    fn clone(&self) -> Self {
        TlsStream {
            stream: Arc::clone(&self.stream),
        }
    }
}

impl<C, T> Write for TlsStream<C, T>
where
    C: Connection,
    T: Read + Write,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let stream = &mut self.stream.lock().unwrap();

        stream.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let stream = &mut self.stream.lock().unwrap();

        stream.flush()?;

        Ok(())
    }
}

impl<C, T> Read for TlsStream<C, T>
where
    C: Connection,
    T: Read + Write,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let stream = &mut self.stream.lock().unwrap();

        stream.read(buf)
    }
}

impl<C, T> TlsStream<C, T>
where
    C: Connection,
    T: Read + Write,
{
    pub fn client_certs(&self) -> Result<Option<Vec<Certificate>>, io::Error> {
        let lk = self.stream.lock().unwrap();
        match lk.conn.peer_certificates() {
            Some(c) => Ok(Some(c.to_vec())),
            None => Ok(None),
        }
    }
}

pub fn create_stream<S: Read + Write>(
    sock: S,
    config: &TlsConfig,
) -> Result<TlsStream<ServerConnection, S>, io::Error> {
    let config = make_config(config)?;

    let conn = ServerConnection::new(config).unwrap();

    let stream = Arc::new(Mutex::new(rustls::StreamOwned { conn, sock }));

    Ok(TlsStream { stream })
}
