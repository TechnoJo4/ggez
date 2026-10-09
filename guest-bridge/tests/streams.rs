use guest_bridge::{handshake, relay};
use std::{io, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

#[tokio::test]
async fn acknowledgement_does_not_consume_rpc_payload() {
    let (mut bridge, mut firecracker) = duplex(64);
    let peer = tokio::spawn(async move {
        let mut command = [0; 13];
        firecracker.read_exact(&mut command).await.unwrap();
        assert_eq!(&command, b"CONNECT 5003\n");
        firecracker.write_all(b"O").await.unwrap();
        tokio::task::yield_now().await;
        firecracker
            .write_all(b"K 1073741824\n\0\xffRPC")
            .await
            .unwrap();
    });
    assert_eq!(handshake(&mut bridge, 5003).await.unwrap(), 1073741824);
    let mut payload = Vec::new();
    bridge.read_to_end(&mut payload).await.unwrap();
    assert_eq!(payload, b"\0\xffRPC");
    peer.await.unwrap();
}

#[tokio::test]
async fn invalid_or_missing_acknowledgements_fail() {
    for reply in [
        b"NO 5003\n".as_slice(),
        b"OK 0\n",
        b"OK -1\n",
        b"OK 4294967296\n",
        b"OK 1\r\n",
        b"OK 1 extra\n",
        b"OK 12345678901234567890\n",
        b"OK 3",
    ] {
        let (mut bridge, mut firecracker) = duplex(64);
        firecracker.write_all(reply).await.unwrap();
        firecracker.shutdown().await.unwrap();
        assert!(
            handshake(&mut bridge, 5003).await.is_err(),
            "accepted {reply:?}"
        );
    }
}

#[tokio::test]
async fn binary_request_half_close_still_receives_response() {
    let (mut client, bridge_client) = duplex(32);
    let (bridge_guest, mut guest) = duplex(32);
    let bridge = tokio::spawn(relay(bridge_client, bridge_guest, Duration::from_secs(5)));
    let request: Vec<u8> = (0..100_000).map(|n| n as u8).collect();
    let expected = request.clone();
    let server = tokio::spawn(async move {
        let mut received = Vec::new();
        guest.read_to_end(&mut received).await.unwrap();
        assert_eq!(received, expected);
        guest.write_all(b"response after EOF\0\xff").await.unwrap();
        guest.shutdown().await.unwrap();
    });
    client.write_all(&request).await.unwrap();
    client.shutdown().await.unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    assert_eq!(response, b"response after EOF\0\xff");
    server.await.unwrap();
    assert_eq!(
        bridge.await.unwrap().unwrap(),
        (100_000, response.len() as u64)
    );
}

#[tokio::test(start_paused = true)]
async fn idle_timeout_tracks_both_directions_together() {
    let (mut client, bridge_client) = duplex(32);
    let (bridge_guest, mut guest) = duplex(32);
    let bridge = tokio::spawn(relay(bridge_client, bridge_guest, Duration::from_secs(10)));
    for _ in 0..4 {
        tokio::time::advance(Duration::from_secs(6)).await;
        guest.write_all(b"x").await.unwrap();
        assert_eq!(client.read_u8().await.unwrap(), b'x');
        assert!(!bridge.is_finished());
    }
    tokio::time::advance(Duration::from_secs(11)).await;
    assert_eq!(
        bridge.await.unwrap().unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[tokio::test(start_paused = true)]
async fn stalled_firecracker_handshake_has_a_deadline() {
    let (mut bridge, _firecracker) = duplex(64);
    let result = tokio::time::timeout(Duration::from_secs(1), handshake(&mut bridge, 5003)).await;
    assert!(result.is_err());
}
