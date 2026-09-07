//! SPIKE — temporary, to prove the tokio-reactor path before building on it.
uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum SpikeError {
    #[error("http: {message}")]
    Http { message: String },
}

/// A real HTTP GET through reqwest, exported async. If `async_runtime = "tokio"` is not doing its
/// job, this is where it shows: reqwest needs a reactor and Swift's executor is not one.
#[uniffi::export(async_runtime = "tokio")]
pub async fn spike_get(url: String) -> Result<String, SpikeError> {
    let body = reqwest::get(&url)
        .await
        .map_err(|e| SpikeError::Http { message: e.to_string() })?
        .text()
        .await
        .map_err(|e| SpikeError::Http { message: e.to_string() })?;
    Ok(body.chars().take(80).collect())
}
