use blockstore::{Blockstore, InMemoryBlockstore};
use futures_util::future::FutureExt;
use futures_util::poll;
use tokio::time::{sleep, Duration};

use crate::utils::{cid, spawn_node};

mod utils;

#[tokio::test]
async fn test_client_request() {
    let data_with_cid = ["foo", "bar", "baz"].map(|d| (cid(d.as_bytes()), d));
    let store = InMemoryBlockstore::new();
    for (cid, data) in data_with_cid {
        store.put_keyed(&cid, data.as_bytes()).await.unwrap()
    }
    let server = spawn_node(Some(store)).await;
    let mut client = spawn_node(None).await;

    let received0 = client.request_cid(data_with_cid[0].0);

    drop(client.connect(&server));
    let received1 = client
        .request_cid(data_with_cid[1].0)
        .await
        .expect("could not get CID");
    let received0 = received0.await.expect("could not get CID");

    assert_eq!(&received0[..], data_with_cid[0].1.as_bytes());
    assert_eq!(&received1[..], data_with_cid[1].1.as_bytes());
}

#[tokio::test]
async fn test_server_request() {
    let data_with_cid = ["foo", "bar", "baz"].map(|d| (cid(d.as_bytes()), d));
    let store = InMemoryBlockstore::new();
    for (cid, data) in data_with_cid {
        store.put_keyed(&cid, data.as_bytes()).await.unwrap()
    }

    let mut client = spawn_node(Some(store)).await;
    let mut server = spawn_node(None).await;

    let received0 = client.request_cid(data_with_cid[0].0);

    drop(client.connect(&server));
    let received1 = server
        .request_cid(data_with_cid[1].0)
        .await
        .expect("could not get CID");
    let received0 = received0.await.expect("Could not get CID");

    assert_eq!(&received0[..], data_with_cid[0].1.as_bytes());
    assert_eq!(&received1[..], data_with_cid[1].1.as_bytes());
}

#[tokio::test]
async fn test_chain_of_nodes() {
    let data = "foo";
    let cid = cid(data.as_bytes());
    let store = InMemoryBlockstore::new();
    store.put_keyed(&cid, data.as_ref()).await.unwrap();

    let mut node_with_data = spawn_node(Some(store)).await;
    let mut node0 = spawn_node(None).await;
    let mut node1 = spawn_node(None).await;
    let mut node2 = spawn_node(None).await;

    drop(node_with_data.connect(&node0));
    drop(node0.connect(&node1));
    drop(node1.connect(&node2));

    let mut node2_request = node2.request_cid(cid);
    sleep(Duration::from_millis(300)).await;
    assert!(poll!(&mut node2_request).is_pending());

    let mut node1_request = node1.request_cid(cid);
    sleep(Duration::from_millis(300)).await;
    assert!(poll!(&mut node1_request).is_pending());
    assert!(poll!(&mut node2_request).is_pending());

    let node0_request = node0.request_cid(cid);
    sleep(Duration::from_millis(300)).await;
    let data0 = node0_request
        .now_or_never()
        .expect("data should be ready")
        .unwrap();
    let data1 = node1_request
        .now_or_never()
        .expect("data should be ready")
        .unwrap();
    let data2 = node2_request
        .now_or_never()
        .expect("data should be ready")
        .unwrap();

    assert_eq!(data0, data.as_bytes());
    assert_eq!(data1, data.as_bytes());
    assert_eq!(data2, data.as_bytes());
}

#[tokio::test]
async fn test_node_with_data_coming_online() {
    let data = "foo";
    let cid = cid(data.as_bytes());
    let store = InMemoryBlockstore::new();
    store.put_keyed(&cid, data.as_ref()).await.unwrap();

    let node_with_data = spawn_node(Some(store)).await;

    let mut node0 = spawn_node(None).await;
    let mut node1 = spawn_node(None).await;
    drop(node0.connect(&node1));

    let mut node0_request = node0.request_cid(cid);
    let mut node1_request = node1.request_cid(cid);
    sleep(Duration::from_millis(300)).await;
    assert!(poll!(&mut node0_request).is_pending());
    assert!(poll!(&mut node1_request).is_pending());

    node1
        .connect(&node_with_data)
        .await
        .expect("could not connect");
    sleep(Duration::from_millis(300)).await;

    let data0 = node0_request
        .now_or_never()
        .expect("data should be ready")
        .unwrap();
    let data1 = node1_request
        .now_or_never()
        .expect("data should be ready")
        .unwrap();
    assert_eq!(data0, data.as_bytes());
    assert_eq!(data1, data.as_bytes());
}

#[tokio::test]
async fn test_node_with_invalid_data() {
    let data = "foo";
    let cid = cid(data.as_bytes());
    let store = InMemoryBlockstore::new();
    store.put_keyed(&cid, data.as_ref()).await.unwrap();

    let invalid_data = "bar";
    let invalid_store = InMemoryBlockstore::new();
    invalid_store
        .put_keyed(&cid, invalid_data.as_ref())
        .await
        .unwrap();

    let mut client = spawn_node(None).await;
    let node = spawn_node(Some(store)).await;
    let malicious_node = spawn_node(Some(invalid_store)).await;

    client
        .connect(&malicious_node)
        .await
        .expect("could not connect");
    let mut request = client.request_cid(cid);
    sleep(Duration::from_millis(200)).await;
    assert!(poll!(&mut request).is_pending());

    drop(client.connect(&node));
    let received = request.await.expect("could not get CID");
    assert_eq!(received, data.as_bytes());
}

#[tokio::test]
async fn test_node_wishlist_updates() {
    let data_with_cid = ["foo", "bar", "baz"].map(|d| (cid(d.as_bytes()), d));
    let store = InMemoryBlockstore::new();
    for (cid, data) in data_with_cid {
        store.put_keyed(&cid, data.as_bytes()).await.unwrap()
    }

    let server = spawn_node(Some(store)).await;
    let mut client = spawn_node(None).await;

    let request0 = client.request_cid(data_with_cid[0].0);
    client.connect(&server).await.unwrap();

    let request1 = client.request_cid(data_with_cid[1].0);

    let data0 = request0.await.unwrap();
    assert_eq!(data0, data_with_cid[0].1.as_bytes());

    let request2 = client.request_cid(data_with_cid[2].0);

    let data1 = request1.await.unwrap();
    let data2 = request2.await.unwrap();

    assert_eq!(data1, data_with_cid[1].1.as_bytes());
    assert_eq!(data2, data_with_cid[2].1.as_bytes());
}
