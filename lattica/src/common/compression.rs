use serde::{Serialize, Deserialize};
use bincode::{Encode, Decode};
use async_compression::Level;
use async_compression::tokio::bufread::{
    GzipEncoder, GzipDecoder,
    ZstdEncoder, ZstdDecoder,
    Lz4Encoder, Lz4Decoder,
    BrotliEncoder, BrotliDecoder,
};
use tokio::io::{AsyncReadExt, BufReader};
use std::io::{Cursor, Result as IoResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Encode, Decode)]
pub enum CompressionAlgorithm {
    None,
    Gzip,
    Zstd,
    Lz4,
    Brotli,
}

impl Default for CompressionAlgorithm {
    fn default() -> Self {
        CompressionAlgorithm::None
    }
}

impl std::fmt::Display for CompressionAlgorithm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompressionAlgorithm::None => write!(f, "none"),
            CompressionAlgorithm::Gzip => write!(f, "gzip"),
            CompressionAlgorithm::Zstd => write!(f, "zstd"),
            CompressionAlgorithm::Lz4 => write!(f, "lz4"),
            CompressionAlgorithm::Brotli => write!(f, "brotli"),
        }
    }
}

pub type CompressionLevel = Level;

// get algorithm min compress size
fn get_min_compress_size(algorithm: CompressionAlgorithm) -> usize {
    match algorithm {
        CompressionAlgorithm::None => usize::MAX,
        CompressionAlgorithm::Lz4 => 256,
        CompressionAlgorithm::Zstd => 512,
        CompressionAlgorithm::Gzip => 512,
        CompressionAlgorithm::Brotli => 1024,
    }
}

// check need to compress
pub fn should_compress(data_size: usize, algorithm: CompressionAlgorithm) -> bool {
    algorithm != CompressionAlgorithm::None && data_size >= get_min_compress_size(algorithm)
}


pub async fn compress_data(
    data: &[u8],
    algorithm: CompressionAlgorithm,
    level: CompressionLevel
) -> IoResult<Vec<u8>> {
    if algorithm == CompressionAlgorithm::None || data.len() < get_min_compress_size(algorithm) {
        return Ok(data.to_vec());
    }

    let reader = BufReader::new(Cursor::new(data));
    let mut result = Vec::new();

    match algorithm {
        CompressionAlgorithm::None => Ok(data.to_vec()),
        CompressionAlgorithm::Gzip => {
            let mut encoder = GzipEncoder::with_quality(reader, level);
            encoder.read_to_end(&mut result).await?;
            Ok(result)
        }
        CompressionAlgorithm::Zstd => {
            let mut encoder = ZstdEncoder::with_quality(reader, level);
            encoder.read_to_end(&mut result).await?;
            Ok(result)
        }
        CompressionAlgorithm::Lz4 => {
            let mut encoder = Lz4Encoder::new(reader);
            encoder.read_to_end(&mut result).await?;
            Ok(result)
        }
        CompressionAlgorithm::Brotli => {
            let mut encoder = BrotliEncoder::with_quality(reader, level);
            encoder.read_to_end(&mut result).await?;
            Ok(result)
        }
    }
}

pub async fn decompress_data(
    data: &[u8],
    algorithm: CompressionAlgorithm
) -> IoResult<Vec<u8>> {
    if algorithm == CompressionAlgorithm::None {
        return Ok(data.to_vec());
    }

    let reader = BufReader::new(Cursor::new(data));
    let mut result = Vec::new();

    match algorithm {
        CompressionAlgorithm::None => Ok(data.to_vec()),
        CompressionAlgorithm::Gzip => {
            let mut decoder = GzipDecoder::new(reader);
            decoder.read_to_end(&mut result).await?;
            Ok(result)
        }
        CompressionAlgorithm::Zstd => {
            let mut decoder = ZstdDecoder::new(reader);
            decoder.read_to_end(&mut result).await?;
            Ok(result)
        }
        CompressionAlgorithm::Lz4 => {
            let mut decoder = Lz4Decoder::new(reader);
            decoder.read_to_end(&mut result).await?;
            Ok(result)
        }
        CompressionAlgorithm::Brotli => {
            let mut decoder = BrotliDecoder::new(reader);
            decoder.read_to_end(&mut result).await?;
            Ok(result)
        }
    }
}