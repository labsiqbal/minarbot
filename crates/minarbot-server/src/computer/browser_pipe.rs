use std::{io, os::fd::OwnedFd, process::Stdio};
use tokio::{net::UnixStream, process::Command};

// Anonymous sockets cannot be reopened through /proc/<pid>/fd like named files or pipes.
pub(super) fn private_stdio(command: &mut Command) -> io::Result<(UnixStream, UnixStream)> {
    fn pair() -> io::Result<(UnixStream, OwnedFd)> {
        let (parent, child) = std::os::unix::net::UnixStream::pair()?;
        parent.set_nonblocking(true)?;
        Ok((UnixStream::from_std(parent)?, child.into()))
    }
    let (send, input) = pair()?;
    let (receive, output) = pair()?;
    command
        .stdin(Stdio::from(input))
        .stdout(Stdio::from(output));
    Ok((send, receive))
}
