use anyhow::{Context, Result};
use bytecodec::{DecodeExt, EncodeExt};
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};
use std::time::Duration;
use stun_codec::rfc5389::attributes::{MappedAddress, XorMappedAddress};
use stun_codec::rfc5389::methods::BINDING;
use stun_codec::rfc5389::Attribute;
use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId};
use std::{sync::Arc};
use tokio::sync::{RwLock};

pub fn check_symmetric_nat(symmetric_nat_clone: Arc<RwLock<Option<bool>>>) -> Result<()> {
    tokio::spawn(async move {
        let max_retries = 5;
        let mut retry_delay = Duration::from_secs(2);
        let stun_servers = vec![
            "stun.l.google.com:19302",
            "stun.cloudflare.com:3478",
        ];

        for attempt in 1..=max_retries {
            let stun_servers_clone = stun_servers.clone();
            match tokio::task::spawn_blocking(move || {
                let detector = match NatDetector::new("0.0.0.0:0") {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::debug!("Failed to create NatDetector: {:?}", e);
                        return None;
                    }
                };

                let mapped_addr1 = match detector.get_mapped_address(stun_servers_clone[0]) {
                    Ok(Some(addr)) => {
                        addr
                    }
                    Err(err) => {
                        tracing::debug!("failed to get mapped address error: {:?}", err);
                        return None
                    }
                    _ => {
                        return None
                    }
                };

                let mapped_addr2 = match detector.get_mapped_address(stun_servers_clone[1]) {
                    Ok(Some(addr)) => {
                        addr
                    }
                    Err(err) => {
                        tracing::debug!("failed to get mapped address error: {:?}", err);
                        return None
                    }
                    _ => {
                        return None
                    }
                };

                // local check
                let local_addr = match detector.local_socket.local_addr() {
                    Ok(addr) => addr,
                    Err(_) => return None,
                };
                if local_addr == mapped_addr1 {
                    return Some(false);
                }

                // symmetric check
                if mapped_addr1.port() != mapped_addr2.port() {
                    return Some(true);
                }

                return Some(false);
            }).await {
                Ok(Some(is_symmetric)) => {
                    match symmetric_nat_clone.try_write() {
                        Ok(mut nat) => {
                            *nat = Some(is_symmetric);
                            tracing::debug!("NAT check completed on attempt {}: symmetric={}", attempt, is_symmetric);
                        }
                        Err(_) => {
                            tracing::debug!("Failed to acquire write lock for symmetric_nat");
                        }
                    }
                    return;
                }
                Ok(None) => {
                    tracing::debug!("NAT check returned None (attempt {}), retrying...", attempt);
                }
                Err(e) => {
                    tracing::debug!("NAT check task error (attempt {}): {}", attempt, e);
                }
            }

            if attempt < max_retries {
                tokio::time::sleep(retry_delay).await;
                retry_delay *= 2;
            }
        }
    });

    Ok(())
}

struct NatDetector {
    local_socket: UdpSocket,
}

impl NatDetector {
    fn new(local_addr: &str) -> Result<Self> {
        let socket = UdpSocket::bind(local_addr)
            .context("Failed to bind UDP socket")?;
        socket.set_read_timeout(Some(Duration::from_secs(5)))?;
        socket.set_write_timeout(Some(Duration::from_secs(5)))?;

        Ok(Self {
            local_socket: socket,
        })
    }

    fn get_mapped_address(&self, stun_server: &str) -> Result<Option<SocketAddr>> {
        let server_addr = stun_server.to_socket_addrs()
            .context("Failed to resolve STUN server address")?
            .next()
            .context("No address found for STUN server")?;

        let message = Message::<Attribute>::new(
            MessageClass::Request,
            BINDING,
            TransactionId::new(rand::random()),
        );

        let mut encoder = MessageEncoder::new();
        let bytes = encoder.encode_into_bytes(message)
            .context("Failed to encode STUN message")?;

        self.local_socket.send_to(&bytes, server_addr)
            .context("Failed to send STUN request")?;

        let mut buf = vec![0u8; 1500];
        let (size, _) = self.local_socket.recv_from(&mut buf)
            .context("Failed to receive STUN response")?;

        let mut decoder = MessageDecoder::<Attribute>::new();
        if let Ok(Ok(msg)) = decoder.decode_from_bytes(&buf[..size]) {
            if let Some(xor_mapped) = msg.get_attribute::<XorMappedAddress>() {
                return Ok(Some(xor_mapped.address()));
            }
            if let Some(mapped) = msg.get_attribute::<MappedAddress>() {
                return Ok(Some(mapped.address()));
            }
        }

        Ok(None)
    }
}