use std::{cell::RefCell, rc::Rc};

use bytes::{Bytes, BytesMut};
use rand::prelude::*;
use rand_chacha::ChaCha8Rng;
use socket2::{Domain, Socket, Type};
use std::os::fd::AsRawFd;

use std::path::Path;

const TEST_DATABASE_NAME: &str = "test";
const TEST_DATABASE_HOST: &str = "test.localhost";

pub struct UserData {
    rng: RefCell<ChaCha8Rng>,
}

type Context = hiisi::server::Context<UserData>;

type IO = hiisi::server::IO<UserData>;

pub fn main() {
    init_logger();

    let seed = match std::env::var("SEED") {
        Ok(seed) => seed.parse::<u64>().unwrap(),
        Err(_) => rand::thread_rng().next_u64(),
    };

    log::info!("Starting simulation with seed {}", seed);

    let rng = ChaCha8Rng::seed_from_u64(seed);
    let user_data = UserData {
        rng: RefCell::new(rng),
    };
    let manager = Rc::new(hiisi::manager::ResourceManager::new(Path::new("data")));
    // TODO: Use the admin interface to create the database as part of simulation.
    manager.create_database(TEST_DATABASE_NAME).unwrap();
    let ctx = Context::new(manager, user_data);
    let mut io = hiisi::server::IO::new(ctx);

    let server_addr: std::net::SocketAddr = "127.0.0.1:8080".parse().unwrap();
    let server_sock = Rc::new(Socket::new(Domain::IPV4, Type::STREAM, None).unwrap());
    let client_sock = Rc::new(Socket::new(Domain::IPV4, Type::STREAM, None).unwrap());

    // Bind the server socket to the server address.
    hiisi::server::serve(&mut io, server_sock, server_addr.clone().into());

    // Connect the client socket to the server address.
    io.connect(client_sock, server_addr.clone().into(), on_client_connect);

    // Main simulation loop.
    loop {
        io.run_once();
    }
}

fn on_client_connect(io: &mut IO, sock: Rc<socket2::Socket>, client_addr: socket2::SockAddr) {
    let sockfd = sock.as_raw_fd();
    log::trace!("Client is connected to {}", sockfd);
    perform_client_req(io, sock);
}

fn perform_client_req(io: &mut IO, sock: Rc<Socket>) {
    let req = hiisi::proto::StreamRequest::Execute(hiisi::proto::ExecuteStreamReq {
        stmt: hiisi::proto::Stmt {
            sql: Some("SELECT 1".to_owned()),
            sql_id: None,
            args: vec![],
            named_args: vec![],
            want_rows: Some(true),
            replication_index: None,
        },
    });
    let req = hiisi::proto::PipelineReqBody {
        baton: None,
        requests: vec![req],
    };
    let buf = hiisi::proto::format_msg(&req).unwrap();
    let mut http_req = BytesMut::new();
    let http_header = format!(
        "POST /v2/pipeline HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\n\r\n",
        TEST_DATABASE_HOST,
        buf.len()
    );
    http_req.extend_from_slice(http_header.as_bytes());
    http_req.extend_from_slice(&buf);
    let n = http_req.len();
    send_client_msg(io, sock, http_req.into(), n);
}

fn send_client_msg(io: &mut IO, sock: Rc<socket2::Socket>, buf: Bytes, n: usize) {
    match gen_perform_client_req_fault(io.context()) {
        PerformClientReqFault::Normal => {
            io.send(sock, buf, n, on_client_send_normal);
        }
        PerformClientReqFault::Fuzz => {
            let bad_request = Bytes::from_static(b"FUZZ FUZZ FUZZ"); // Fuzzed request.
            io.send(sock, bad_request, n, on_client_send_fuzz);
        }
    }
}

enum PerformClientReqFault {
    // Client sends a normal message to the server.
    Normal,
    // Client sends a fuzzed message to the server.
    Fuzz,
}

fn gen_perform_client_req_fault(ctx: &hiisi::server::Context<UserData>) -> PerformClientReqFault {
    let user_data = &ctx.user_data;
    let mut rng = user_data.rng.borrow_mut();
    if rng.gen_bool(0.9) {
        PerformClientReqFault::Normal
    } else {
        PerformClientReqFault::Fuzz
    }
}

fn on_client_send_normal(io: &mut IO, server_sock: Rc<socket2::Socket>, n: usize) {
    io.recv(server_sock, on_client_recv_normal);
}

fn on_client_recv_normal(io: &mut IO, socket: Rc<socket2::Socket>, buf: &[u8], n: usize) {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut headers);
    let body_off = resp.parse(buf).unwrap().unwrap();
    if resp.code.unwrap() != 200 {
        let body = std::str::from_utf8(&buf[body_off..]).unwrap();
        println!("Error: {:?} -> {}", resp, body);
        assert_eq!(resp.code.unwrap(), 200);
    }
    perform_client_req(io, socket);
}

fn on_client_send_fuzz(io: &mut IO, server_sock: Rc<socket2::Socket>, n: usize) {
    io.recv(server_sock, on_client_recv_fuzz);
}

fn on_client_recv_fuzz(io: &mut IO, socket: Rc<socket2::Socket>, buf: &[u8], n: usize) {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut resp = httparse::Response::new(&mut headers);
    let body_off = resp.parse(buf).unwrap().unwrap();
    if resp.code.unwrap() != 400 {
        let body = std::str::from_utf8(&buf[body_off..]).unwrap();
        println!("Error: {:?} -> {}", resp, body);
        assert_eq!(resp.code.unwrap(), 400);
    }
    perform_client_req(io, socket);
}

fn init_logger() {
    let env = env_logger::Env::default().default_filter_or("info");
    env_logger::Builder::from_env(env).init();
}
