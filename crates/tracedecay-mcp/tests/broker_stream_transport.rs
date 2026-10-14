use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tracedecay_daemon_protocol::{BrokerListener, DaemonEndpoint, default_loopback_endpoint};
use tracedecay_mcp::{BrokerStreamTransport, McpTransport};

#[tokio::test]
async fn tcp_broker_transport_preserves_bidirectional_line_framing() {
    let (listener, endpoint) = BrokerListener::bind(&default_loopback_endpoint())
        .await
        .expect("bind loopback listener");
    let DaemonEndpoint::Loopback(address) = endpoint else {
        panic!("loopback bind returned {endpoint}");
    };
    let mut client = tokio::net::TcpStream::connect(address)
        .await
        .expect("connect loopback client");
    let server = listener.accept().await.expect("accept loopback client");
    let mut transport = BrokerStreamTransport::new(server);

    client
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n")
        .await
        .expect("write request line");
    assert_eq!(
        transport.read_line().await.expect("read request line"),
        Some("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}".to_owned())
    );

    transport
        .write_line("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
        .await
        .expect("write response line");
    transport.flush().await.expect("flush response line");

    let mut response = String::new();
    BufReader::new(client)
        .read_line(&mut response)
        .await
        .expect("read response line");
    assert_eq!(response, "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n");
}
