//! Example websocket server.
//!
//! Run the server with
//! ```not_rust
//! cargo run -p example-websockets --bin example-websockets
//! ```
//!
//! Run a browser client with
//! ```not_rust
//! firefox http://localhost:3000
//! ```
//!
//! Alternatively you can run the rust client (showing two
//! concurrent websocket connections being established) with
//! ```not_rust
//! cargo run -p example-websockets --bin example-client
//! ```

use axum::{
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    response::IntoResponse,
    routing::get,
    Router,
};
use axum_extra::TypedHeader;

use chrono::Local;
use std::borrow::Cow;
use std::io;
use std::ops::ControlFlow;
use std::{net::SocketAddr, path::PathBuf};
use tokio::{
    fs::File,
    io::{BufReader, BufWriter},
};
use tower_http::{
    services::ServeDir,
    trace::{DefaultMakeSpan, TraceLayer},
};

use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

//allows to extract the IP of connecting user
use axum::extract::connect_info::ConnectInfo;
use axum::extract::ws::CloseFrame;

//allows to split the websocket stream into separate TX and RX branches
use futures::{sink::SinkExt, stream::StreamExt};

const UPLOADS_DIRECTORY: &str = "uploads-websocket";

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=debug,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let assets_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets");

    // build our application with some routes
    let app = Router::new()
        .fallback_service(ServeDir::new(assets_dir).append_index_html_on_directories(true))
        .route("/ws", get(ws_handler))
        // logging so we can see whats going on
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );

    // run it with hyper
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3003").await.unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

/// The handler for the HTTP request (this gets called when the HTTP GET lands at the start
/// of websocket negotiation). After this completes, the actual switching from HTTP to
/// websocket protocol will occur.
/// This is the last point where we can extract TCP/IP metadata such as IP address of the client
/// as well as things from HTTP headers such as user-agent of the browser etc.
async fn ws_handler(
    ws: WebSocketUpgrade,
    user_agent: Option<TypedHeader<headers::UserAgent>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
) -> impl IntoResponse {
    let user_agent = if let Some(TypedHeader(user_agent)) = user_agent {
        user_agent.to_string()
    } else {
        String::from("Unknown browser")
    };
    println!("`{user_agent}` at {addr} connected.");
    // finalize the upgrade process by returning upgrade callback.
    // we can customize the callback by sending additional info such as address.
    ws.on_upgrade(move |socket| handle_socket(socket, addr))
}

/// Actual websocket statemachine (one will be spawned per connection)
async fn handle_socket(mut socket: WebSocket, who: SocketAddr) {
    let mut serial_number = String::from("undefined");
    // send a ping (unsupported by some browsers) just to kick things off and get a response
    if socket.send(Message::Ping(vec![1, 2, 3])).await.is_ok() {
        println!("Pinged {who}...");
    } else {
        println!("Could not send ping {who}!");
        // no Error here since the only thing we can do is to close the connection.
        // If we can not send messages, there is no way to salvage the statemachine anyway.
        return;
    }

    if let Some(msg) = socket.recv().await {
        if let Ok(msg) = msg {
            let message = get_text(msg, who);
            if message.is_break() {
                return;
            } else {
                if let Some(text) = message.continue_value() {
                    serial_number = text.clone();
                }
            }
        } else {
            println!("client {who} abruptly disconnected");
            return;
        }
    }

    // receive single message from a client (we can either receive or send with socket).
    // this will likely be the Pong for our Ping or a hello message from client.
    // waiting for message from a client will block this task, but will not block other client's
    // connections.
    while let Some(msg) = socket.recv().await {
        if let Ok(msg) = msg {
            if process_message(msg, who, &serial_number).await.is_break() {
                return;
            }
        } else {
            println!("client {who} abruptly disconnected");
            return;
        }
    }

    // returning from the handler closes the websocket connection
    println!("Websocket context {who} destroyed");
}

fn get_text(msg: Message, who: SocketAddr) -> ControlFlow<(), String> {
    match msg {
        Message::Text(text) => {
            println!(">>> {who} sent str: {text:?}");
            ControlFlow::Continue(text)
        }
        Message::Close(c) => {
            if let Some(cf) = c {
                println!(
                    ">>> {} sent close with code {} and reason `{}`",
                    who, cf.code, cf.reason
                );
            } else {
                println!(">>> {who} somehow sent close message without CloseFrame");
            }
            return ControlFlow::Break(());
        }
        _ => {
            println!("unexpected message");
            return ControlFlow::Break(());
        }
    }
}

/// helper to print contents of messages to stdout. Has special treatment for Close.
async fn process_message(
    msg: Message,
    who: SocketAddr,
    serial_number: &str,
) -> ControlFlow<(), ()> {
    match msg {
        Message::Text(t) => {
            println!(">>> {who} sent str: {t:?}");
        }
        Message::Binary(d) => {
            println!(">>> {} sent {} bytes: {:?}", who, d.len(), d);
            println!("going to save received image to file");
            let _ = save_image(serial_number, d).await;
            // to do
            // save the image to disk
        }
        Message::Close(c) => {
            if let Some(cf) = c {
                println!(
                    ">>> {} sent close with code {} and reason `{}`",
                    who, cf.code, cf.reason
                );
            } else {
                println!(">>> {who} somehow sent close message without CloseFrame");
            }
            return ControlFlow::Break(());
        }

        Message::Pong(v) => {
            println!(">>> {who} sent pong with {v:?}");
        }
        // You should never need to manually handle Message::Ping, as axum's websocket library
        // will do so for you automagically by replying with Pong and copying the v according to
        // spec. But if you need the contents of the pings you can see them here.
        Message::Ping(v) => {
            println!(">>> {who} sent ping with {v:?}");
        }
    }
    ControlFlow::Continue(())
}

async fn save_image(serial_number: &str, data: Vec<u8>) -> Result<(), String> {
    async {
        let local_time = Local::now().format("%Y%m%d-%H%M%S");
        let filename = format!("image-{}.jpg", local_time);

        tokio::fs::create_dir_all(format!("{}/{}", UPLOADS_DIRECTORY, serial_number))
            .await
            .expect("failed to create `uploads/<serial_number>` directory");

        let path = format!("{}/{}", serial_number, filename);

        // Create the file. `File` implements `AsyncWrite`.
        let path_buf = std::path::Path::new(UPLOADS_DIRECTORY).join(&path);
        let mut file = BufWriter::new(File::create(path_buf).await?);

        // Copy the body into the file.
        tokio::io::copy(&mut data.as_slice(), &mut file).await?;

        // Read the file just copied
        let path_buf = std::path::Path::new(UPLOADS_DIRECTORY).join(&path);
        let mut image_file = BufReader::new(File::open(path_buf).await?);

        let filename_latest = "aaa-latest.jpg";
        let path_latest = format!("{}/{}", serial_number, filename_latest);

        // Create the file. `File` implements `AsyncWrite`.
        let path_latest_buf = std::path::Path::new(UPLOADS_DIRECTORY).join(&path_latest);
        let mut file_latest = BufWriter::new(File::create(path_latest_buf).await?);

        // Copy the image file into the latest file.
        tokio::io::copy(&mut image_file, &mut file_latest).await?;
        tracing::debug!(
            "image saved to {}: {} and {}",
            serial_number,
            filename,
            filename_latest
        );

        Ok::<_, io::Error>(())
    }
    .await
    .map_err(|err| err.to_string())
}
