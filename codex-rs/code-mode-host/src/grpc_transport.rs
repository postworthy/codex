use std::io;
use std::io::Write as _;
use std::net::SocketAddr;

use anyhow::Context;
use anyhow::Result;
use axum::extract::Request;
use axum::http::Method;
use axum::http::StatusCode;
use axum::http::Version;
use axum::middleware;
use axum::middleware::Next;
use axum::routing::get;
use codex_code_mode_protocol::grpc::code_mode_host_server::CodeModeHostServer;
use codex_code_mode_protocol::host::MAX_FRAME_BYTES;
use tokio::net::TcpListener;
use tonic::service::Routes;
use tonic::transport::Server;
use tonic::transport::server::TcpIncoming;
use tracing::info;

use crate::GrpcCodeModeHost;

pub(super) async fn run_tcp_listener(bind_address: SocketAddr) -> Result<()> {
    run_tcp_listener_with_endpoint_scheme(bind_address, "http", "/healthz").await
}

pub(super) async fn run_legacy_source_update_probe(bind_address: SocketAddr) -> Result<()> {
    run_tcp_listener_with_endpoint_scheme(bind_address, "ws", "/readyz").await
}

async fn run_tcp_listener_with_endpoint_scheme(
    bind_address: SocketAddr,
    endpoint_scheme: &str,
    health_path: &'static str,
) -> Result<()> {
    let listener = bind_tcp_listener(bind_address).await?;
    let local_address = listener
        .local_addr()
        .context("failed to read code-mode gRPC listen address")?;
    info!("codex-code-mode-host listening on {endpoint_scheme}://{local_address}");
    println!("{endpoint_scheme}://{local_address}");
    io::stdout()
        .flush()
        .context("failed to publish code-mode gRPC listen address")?;

    let routes = Routes::new(
        CodeModeHostServer::new(GrpcCodeModeHost::new())
            .max_decoding_message_size(MAX_FRAME_BYTES)
            .max_encoding_message_size(MAX_FRAME_BYTES),
    )
    .into_axum_router()
    .route(health_path, get(|| async { StatusCode::OK }))
    .layer(middleware::from_fn(
        move |request: Request, next: Next| async move {
            if request.version() != Version::HTTP_2
                && (request.method() != Method::GET || request.uri().path() != health_path)
            {
                return Err(StatusCode::HTTP_VERSION_NOT_SUPPORTED);
            }

            Ok(next.run(request).await)
        },
    ));

    Server::builder()
        .accept_http1(/*accept_http1*/ true)
        .add_routes(routes.into())
        .serve_with_incoming(listener)
        .await
        .context("code-mode gRPC TCP listener failed")
}

pub(super) async fn bind_tcp_listener(bind_address: SocketAddr) -> Result<TcpIncoming> {
    let listener = TcpListener::bind(bind_address)
        .await
        .with_context(|| format!("failed to bind code-mode gRPC host to {bind_address}"))?;
    Ok(TcpIncoming::from(listener).with_nodelay(Some(true)))
}
