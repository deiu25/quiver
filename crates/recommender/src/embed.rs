use std::path::PathBuf;

use anyhow::Context;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};

pub struct Embedder {
    model: TextEmbedding,
}

impl Embedder {
    pub fn new() -> anyhow::Result<Self> {
        let cache = cache_dir();
        std::fs::create_dir_all(&cache).ok();
        let opts = InitOptions::new(EmbeddingModel::BGESmallENV15)
            .with_cache_dir(cache)
            .with_show_download_progress(false);
        let model = TextEmbedding::try_new(opts).context("init bge-small-en-v1.5 model")?;
        Ok(Self { model })
    }

    pub fn embed_one(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        let mut out = self.model.embed(vec![text.to_string()], None)?;
        out.pop()
            .ok_or_else(|| anyhow::anyhow!("embed() returned no vectors"))
    }

    pub fn embed_batch(&self, texts: Vec<String>) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(self.model.embed(texts, None)?)
    }
}

fn cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .ok()
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|h| PathBuf::from(h).join(".cache"))
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .join("toolhub")
        .join("models")
}
