use super::server::Result;
use axum::Router;
use std::net::{Ipv4Addr, SocketAddr};
use tokio::{net::TcpListener, task::JoinHandle};

pub struct Loopback {
    pub address: SocketAddr,
    task: JoinHandle<std::io::Result<()>>,
}

impl Loopback {
    pub async fn start(routes: Router) -> Result<Self> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        Ok(Self {
            address: listener.local_addr()?,
            task: tokio::spawn(async move { axum::serve(listener, routes).await }),
        })
    }

    pub fn base_url(&self) -> String {
        format!("http://{}/v1", self.address)
    }
}

impl Drop for Loopback {
    fn drop(&mut self) {
        self.task.abort();
    }
}
