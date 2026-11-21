use std::{env, ffi::CString, io::{Read, Write}, net::TcpListener, os::unix::net::UnixStream};

#[link(name = "sqlite3")]
unsafe extern "C" {
    fn sqlite3_open(path: *const i8, db: *mut *mut std::ffi::c_void) -> i32;
    fn sqlite3_exec(db: *mut std::ffi::c_void, sql: *const i8, callback: *const std::ffi::c_void, arg: *mut std::ffi::c_void, error: *mut *mut i8) -> i32;
    fn sqlite3_close(db: *mut std::ffi::c_void) -> i32;
}
fn quote(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let version = env!("MINARBOT_FIXTURE_VERSION");
    let mode = env!("MINARBOT_FIXTURE_MODE");
    // Fails the way the real server's main does when it refuses unmarked data.
    if mode == "refuse" { return Err("This database does not carry the Minarbot data marker. Install fresh.".into()); }
    let file = CString::new(format!("{}/minarbot.sqlite", env::var("MINARBOT_DATA_DIR")?))?;
    let sql = CString::new(if mode == "healthy" {
        "CREATE TABLE IF NOT EXISTS data(value TEXT);CREATE TABLE IF NOT EXISTS turns(status TEXT);"
    } else {
        "DELETE FROM data;INSERT INTO data VALUES('trial damage');"
    })?;
    let mut db = std::ptr::null_mut();
    unsafe {
        if sqlite3_open(file.as_ptr(), &mut db) != 0 { return Err("Fixture database open failed".into()); }
        let result = sqlite3_exec(db, sql.as_ptr(), std::ptr::null(), std::ptr::null_mut(), std::ptr::null_mut());
        sqlite3_close(db);
        if result != 0 { return Err("Fixture database write failed".into()); }
    }
    if mode == "exit" { std::process::exit(1); }
    let listener = TcpListener::bind(format!("127.0.0.1:{}", env::var("PORT")?))?;
    for connection in listener.incoming() {
        let mut connection = connection?;
        let mut request = [0; 4096];
        connection.read(&mut request)?;
        let mut committed = String::from("null");
        if let (Ok(id), Ok(path)) = (env::var("MINARBOT_UPDATE_TRIAL"), env::var("MINARBOT_LAUNCHER_SOCKET")) {
            let mut socket = UnixStream::connect(path)?;
            writeln!(socket, "{{\"type\":\"trial\",\"id\":{}}}", quote(&id))?;
            socket.shutdown(std::net::Shutdown::Write)?;
            let mut response = String::new(); socket.read_to_string(&mut response)?;
            if response.contains("\"committed\":true") { committed = quote(&id); }
        }
        let variables = env::vars().filter(|(k, _)| k.starts_with("LAUNCHER_") || matches!(k.as_str(), "PORT" | "MINARBOT_DATA_DIR" | "MINARBOT_LAUNCHER_SOCKET" | "MINARBOT_UPDATE_TRIAL")).map(|(k,v)| format!("{}:{}",quote(&k),quote(&v))).collect::<Vec<_>>().join(",");
        // A real server that restarted into this release answers a sender's late apply poll
        // with the launcher's attempt ID, which is the receipt ID of the peer request.
        if request.starts_with(b"POST /api/peer/updates/apply ") {
            if let Ok(id) = env::var("MINARBOT_UPDATE_TRIAL") {
                let body = format!("{{\"accepted\":true,\"id\":{}}}", quote(&id));
                write!(connection, "HTTP/1.1 202 Accepted\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body)?;
                continue;
            }
        }
        let body = format!("{{\"version\":{},\"native\":true,\"connected\":true,\"committed\":{committed},\"env\":{{{variables}}}}}", quote(if mode == "wrong" { "0.0.0" } else { version }));
        write!(connection, "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n0\r\n\r\n", body.len(), body)?;
    }
    Ok(())
}
