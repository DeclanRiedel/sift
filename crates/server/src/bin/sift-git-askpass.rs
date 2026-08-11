//! One-operation Git askpass bridge.
//!
//! The socket path is non-secret. Credential bytes travel only over the
//! private Unix socket and stdout directly into Git's askpass pipe.

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use std::io::{Read as _, Write as _};
    use std::os::unix::net::UnixStream;

    let socket = std::env::var_os("SIFT_GIT_ASKPASS_SOCKET")
        .ok_or_else(|| std::io::Error::other("askpass socket is unavailable"))?;
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let kind = if prompt.to_ascii_lowercase().contains("username") {
        b'U'
    } else {
        b'P'
    };
    let mut stream = UnixStream::connect(socket)?;
    stream.write_all(&[kind])?;
    let mut secret = Vec::new();
    stream.take(16 * 1024 + 1).read_to_end(&mut secret)?;
    if secret.len() > 16 * 1024 {
        return Err(std::io::Error::other("askpass response is too large"));
    }
    std::io::stdout().write_all(&secret)?;
    std::io::stdout().write_all(b"\n")?;
    secret.fill(0);
    Ok(())
}

#[cfg(not(unix))]
fn main() {
    std::process::exit(1);
}
