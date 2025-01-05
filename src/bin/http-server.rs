use axum::{
    body::Bytes,
    extract::{Path, Request},
    http::StatusCode,
    response::Html,
    routing::{get, post},
    BoxError, Router,
};
use chrono::Local;
use futures::{Stream, TryStreamExt};
use std::io;
use tokio::{
    fs::File,
    io::{BufReader, BufWriter},
};
use tokio_util::io::StreamReader;
use tower_http::services::ServeDir;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

const UPLOADS_DIRECTORY: &str = "uploads";

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| format!("{}=debug", env!("CARGO_CRATE_NAME")).into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // save files to a separate directory to not override files in the current directory
    tokio::fs::create_dir_all(UPLOADS_DIRECTORY)
        .await
        .expect("failed to create `uploads` directory");

    let app = Router::new()
        .route("/", get(home))
        .route("/upload/:serial_number", post(save_request_body))
        .nest_service("/images", ServeDir::new(UPLOADS_DIRECTORY));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

// Handler that streams the request body to a file.
async fn save_request_body(
    Path(serial_number): Path<String>,
    request: Request,
) -> Result<(), (StatusCode, String)> {
    stream_to_file(&serial_number, request.into_body().into_data_stream()).await
}

// Handler that returns HTML for the home page.
async fn home() -> Html<&'static str> {
    Html(
        r#"
        <!doctype html>
        <html>
            <head>
                <title>Upload images</title>
            </head>
            <body>
                to do
            </body>
        </html>
        "#,
    )
}

// Save a `Stream` to a file
async fn stream_to_file<S, E>(serial_number: &str, stream: S) -> Result<(), (StatusCode, String)>
where
    S: Stream<Item = Result<Bytes, E>>,
    E: Into<BoxError>,
{
    // if !path_is_valid(path) {
    //     return Err((StatusCode::BAD_REQUEST, "Invalid path".to_owned()));
    // }

    async {
        // Convert the stream into an `AsyncRead`.
        let body_with_io_error = stream.map_err(|err| io::Error::new(io::ErrorKind::Other, err));
        let body_reader = StreamReader::new(body_with_io_error);
        futures::pin_mut!(body_reader);

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
        tokio::io::copy(&mut body_reader, &mut file).await?;

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
    .map_err(|err| (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()))
}

// to prevent directory traversal attacks we ensure the path consists of exactly one normal
// component
fn path_is_valid(path: &str) -> bool {
    let path = std::path::Path::new(path);
    let mut components = path.components().peekable();

    if let Some(first) = components.peek() {
        if !matches!(first, std::path::Component::Normal(_)) {
            return false;
        }
    }
    components.count() == 1
}
