//! How a connection travels: straight between the two DevCats, or through a relay.
use devcat_remote_proto::ConnPath;
use iroh::endpoint::Connection;

pub fn conn_path(conn: &Connection) -> Option<ConnPath> {
    let paths = conn.paths();
    let p = paths.iter().find(|p| p.is_selected())?;
    let relay = match p.remote_addr() {
        iroh::TransportAddr::Relay(url) => Some(url.to_string()),
        _ => None,
    };
    Some(ConnPath { direct: p.is_ip(), rtt_ms: p.rtt().as_millis() as u64, relay })
}
