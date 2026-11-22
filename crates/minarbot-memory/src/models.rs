// Hindsight v1.2.3, bd230e8bb40e768bf69f266f1dd3c9ae03e22fe3.
// Model selection: hindsight-api-slim/hindsight_api/config.py.

use crate::{Models, Result, DIMENSIONS};
use candle_core::{DType, Device, Tensor};
use candle_nn::{linear, Linear, Module, VarBuilder};
use candle_transformers::models::bert::{BertModel, Config};
use reqwest::blocking::Client;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
use tokenizers::{EncodeInput, PaddingParams, Tokenizer, TruncationParams};

const TOKENIZER: Asset = Asset(
    "tokenizer.json",
    711396,
    "d241a60d5e8f04cc1b2b3e9ef7a4921b27bf526d9f6050ab90f9267a1f9e5c66",
);
const BGE: Checkpoint = Checkpoint {
    name: "bge-small-en-v1.5",
    base: "https://huggingface.co/BAAI/bge-small-en-v1.5/resolve/5c38ec7c405ec4b44b94cc5a9bb96e735b38267a",
    config: Asset("config.json", 743, "094f8e891b932f2000c92cfc663bac4c62069f5d8af5b5278c4306aef3084750"),
    weights: Asset("model.safetensors", 133466304, "3c9f31665447c8911517620762200d2245a2518d6e7208acc78cd9db317e21ad"),
};
const MINILM: Checkpoint = Checkpoint {
    name: "ms-marco-MiniLM-L6-v2",
    base: "https://huggingface.co/cross-encoder/ms-marco-MiniLM-L6-v2/resolve/233902d25c440f23af6f7d6e94d2946bac0bee0a",
    config: Asset("config.json", 794, "380e02c93f431831be65d99a4e7e5f67c133985bf2e77d9d4eba46847190bacc"),
    weights: Asset("model.safetensors", 90870598, "821d1aa69520101d6e0737f78a042ae25b19e5cb9160701909d10434f4aeb0ae"),
};

struct Asset(&'static str, usize, &'static str);
struct Checkpoint {
    name: &'static str,
    base: &'static str,
    config: Asset,
    weights: Asset,
}

fn read_verified(reader: impl Read, asset: &Asset) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(asset.1);
    reader.take(asset.1 as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() != asset.1 || format!("{:x}", Sha256::digest(&bytes)) != asset.2 {
        return Err(format!("Invalid length or SHA-256 for {}", asset.0).into());
    }
    Ok(bytes)
}

fn asset(client: &Client, base: &str, dir: &Path, expected: &Asset) -> Result<Vec<u8>> {
    let path = dir.join(expected.0);
    match fs::File::open(&path) {
        Ok(file) => {
            if let Ok(bytes) = read_verified(file, expected) {
                return Ok(bytes);
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(err.into()),
    }
    let response = client
        .get(format!("{base}/{}", expected.0))
        .send()?
        .error_for_status()?;
    let bytes = read_verified(response, expected)?;
    let mut temporary = tempfile::NamedTempFile::new_in(dir)?;
    temporary.write_all(&bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path)?;
    Ok(bytes)
}

struct Encoder {
    tokenizer: Tokenizer,
    model: BertModel,
}

impl Encoder {
    fn first_token(&self, inputs: Vec<EncodeInput<'_>>) -> Result<Tensor> {
        let tokens = self.tokenizer.encode_batch(inputs, true)?;
        let width = tokens.first().ok_or("Model batch is empty")?.len();
        let shape = (tokens.len(), width);
        let ids = tokens
            .iter()
            .flat_map(|token| token.get_ids().iter().copied())
            .collect();
        let types = tokens
            .iter()
            .flat_map(|token| token.get_type_ids().iter().copied())
            .collect();
        let mask = tokens
            .iter()
            .flat_map(|token| token.get_attention_mask().iter().copied())
            .collect();
        let ids = Tensor::from_vec::<_, u32>(ids, shape, &Device::Cpu)?;
        let types = Tensor::from_vec::<_, u32>(types, shape, &Device::Cpu)?;
        let mask = Tensor::from_vec::<_, u32>(mask, shape, &Device::Cpu)?;
        let hidden = self.model.forward(&ids, &types, Some(&mask))?;
        Ok(hidden.narrow(1, 0, 1)?.squeeze(1)?)
    }
}

fn load(
    client: &Client,
    root: &Path,
    checkpoint: &Checkpoint,
) -> Result<(Encoder, VarBuilder<'static>, usize)> {
    let dir = root.join(checkpoint.name);
    fs::create_dir_all(&dir)?;
    let config: Config =
        serde_json::from_slice(&asset(client, checkpoint.base, &dir, &checkpoint.config)?)?;
    let mut tokenizer = Tokenizer::from_bytes(asset(client, checkpoint.base, &dir, &TOKENIZER)?)?;
    tokenizer.with_padding(Some(PaddingParams::default()));
    tokenizer.with_truncation(Some(TruncationParams {
        // ponytail: bound CPU inference. Add chunked indexing for long messages.
        max_length: 256,
        ..TruncationParams::default()
    }))?;
    let weights = asset(client, checkpoint.base, &dir, &checkpoint.weights)?;
    let builder = VarBuilder::from_buffered_safetensors(weights, DType::F32, &Device::Cpu)?;
    let model = BertModel::load(builder.clone(), &config)?;
    Ok((Encoder { tokenizer, model }, builder, config.hidden_size))
}

/// CPU inference with a verified on-disk model cache. Load on a blocking worker.
pub struct LocalModels {
    embedding: Encoder,
    reranker: Encoder,
    pooler: Linear,
    classifier: Linear,
}

impl LocalModels {
    pub fn load(directory: &Path) -> Result<Self> {
        let client = Client::builder()
            .https_only(true)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(180))
            .build()?;
        let (embedding, _, _) = load(&client, directory, &BGE)?;
        let (reranker, builder, hidden) = load(&client, directory, &MINILM)?;
        let pooler = linear(hidden, hidden, builder.pp("bert.pooler.dense"))?;
        let classifier = linear(hidden, 1, builder.pp("classifier"))?;
        Ok(Self {
            embedding,
            reranker,
            pooler,
            classifier,
        })
    }
}

impl Models for LocalModels {
    fn embed(&self, text: &[&str], query: bool) -> Result<Vec<[f32; DIMENSIONS]>> {
        const PREFIX: &str = "Represent this sentence for searching relevant passages: ";
        let inputs = text
            .iter()
            .map(|value| {
                if query {
                    format!("{PREFIX}{value}").into()
                } else {
                    (*value).into()
                }
            })
            .collect();
        self.embedding
            .first_token(inputs)?
            .to_vec2::<f32>()?
            .into_iter()
            .map(|vector| {
                let mut vector: [f32; DIMENSIONS] =
                    vector.try_into().map_err(|_| "Unexpected BGE dimension")?;
                let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
                if !norm.is_finite() || norm == 0.0 {
                    return Err("Invalid BGE embedding".into());
                }
                for value in &mut vector {
                    *value /= norm;
                }
                Ok(vector)
            })
            .collect()
    }

    fn rerank(&self, query: &str, passages: &[&str]) -> Result<Vec<f32>> {
        let cls = self.reranker.first_token(
            passages
                .iter()
                .map(|passage| (query, *passage).into())
                .collect(),
        )?;
        let pooled = self.pooler.forward(&cls)?.tanh()?;
        Ok(self.classifier.forward(&pooled)?.flatten_all()?.to_vec1()?)
    }
}
