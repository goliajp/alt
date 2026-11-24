//! The two endpoints a browser signs in to a Cloud instance through, asked with
//! a body that does not come.
use super::*;
use std::io::Write;

#[test]
#[ignore = "requires isolated PostgreSQL and actual loopback HTTP platform fixture"]
fn a_body_that_does_not_come_keeps_no_worker_from_one_who_signs_in_to_a_cloud_instance() {
    let mut f = Fixture::new();
    let platform = Platform::start(&f);
    let Instance { server, .. } = instance(&mut f, &platform, |options| options.workers = 1);
    let agent = ureq::AgentBuilder::new().redirects(0).build();
    let start = agent.get(&format!("{}/web/login/start", server.url)).call();
    let start = start.unwrap();
    let pending = start
        .header("Set-Cookie")
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let location = alt_url::Url::parse(start.header("Location").unwrap()).unwrap();
    let state = location.query_pairs().find(|(name, _)| name == "state");
    let state = state.unwrap().1.into_owned();
    let address = server.url.strip_prefix("http://").unwrap();
    let ticket = "ff".repeat(32);
    let heads = [
        "GET /web/login/start HTTP/1.1\r\n".to_owned(),
        format!(
            "GET /web/login/callback?ticket={ticket}&state={state} HTTP/1.1\r\nCookie: {pending}\r\n"
        ),
    ];
    // each declares a body and sends none of it
    let mut waiting = Vec::new();
    for head in heads {
        let mut socket = std::net::TcpStream::connect(address).unwrap();
        let request = format!("{head}Host: {address}\r\nContent-Length: 1\r\n\r\n");
        socket.write_all(request.as_bytes()).unwrap();
        waiting.push(socket);
    }
    // well within the time a body has, and long past the reading of the two heads
    let until = std::time::Instant::now() + Duration::from_millis(1500);
    while std::time::Instant::now() < until {
        let asked = ureq::get(&format!("{}/healthz", server.url));
        let answered = asked.timeout(Duration::from_secs(30)).call();
        assert_eq!(answered.map(|response| response.status()).ok(), Some(200));
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(platform.exchanges.load(Ordering::SeqCst), 0);
    drop(waiting);
    drop(server);
    platform.end();
}
