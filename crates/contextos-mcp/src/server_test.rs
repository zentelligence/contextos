use super::*;
use crate::{EmbeddingConfig, EmbeddingProvider};
use contextos_search::{ApiKeySource, EmbeddingProviderConfig, REQUIRED_MODEL_FILES};

#[test]
fn local_provider_config_resolves_a_shared_cache_root_model_directory() -> Result<(), Box<dyn std::error::Error>> {
    let cache = tempfile::tempdir()?;
    let repo_dir = cache.path().join("models--Qdrant--all-MiniLM-L6-v2-onnx");
    let snapshot_dir = repo_dir.join("snapshots").join("test-revision");
    std::fs::create_dir_all(&snapshot_dir)?;
    let refs_dir = repo_dir.join("refs");
    std::fs::create_dir_all(&refs_dir)?;
    std::fs::write(refs_dir.join("main"), "test-revision")?;
    for file in REQUIRED_MODEL_FILES {
        std::fs::write(snapshot_dir.join(file), b"fixture")?;
    }

    let embedding = EmbeddingConfig {
        provider: EmbeddingProvider::Local,
        model: String::new(),
        endpoint: String::new(),
        api_key: String::new(),
        api_key_env: String::new(),
        model_directory: Some(cache.path().to_path_buf()),
    };

    let provider_config = embedding_provider_config(&embedding)?;

    let EmbeddingProviderConfig::Local { model_directory } = provider_config else {
        return Err("expected a Local provider config".into());
    };
    assert_eq!(model_directory, snapshot_dir);
    Ok(())
}

fn openai_compatible(api_key: &str, api_key_env: &str) -> EmbeddingConfig {
    EmbeddingConfig {
        provider: EmbeddingProvider::OpenaiCompatible,
        model: "test-model".to_owned(),
        endpoint: "https://embeddings.example.com/v1/embeddings".to_owned(),
        api_key: api_key.to_owned(),
        api_key_env: api_key_env.to_owned(),
        model_directory: None,
    }
}

#[test]
fn an_openai_compatible_key_written_in_the_file_reaches_the_provider_as_a_value()
-> Result<(), Box<dyn std::error::Error>> {
    let provider_config = embedding_provider_config(&openai_compatible("sk-in-the-file", ""))?;

    let EmbeddingProviderConfig::OpenAiCompatible { api_key, .. } = provider_config else {
        return Err("expected an openai-compatible provider config".into());
    };
    assert_eq!(api_key, ApiKeySource::Value("sk-in-the-file".to_owned()));
    Ok(())
}

#[test]
fn an_openai_compatible_key_variable_reaches_the_provider_as_its_name() -> Result<(), Box<dyn std::error::Error>> {
    let provider_config = embedding_provider_config(&openai_compatible("", "OPENAI_API_KEY"))?;

    let EmbeddingProviderConfig::OpenAiCompatible { api_key, .. } = provider_config else {
        return Err("expected an openai-compatible provider config".into());
    };
    assert_eq!(api_key, ApiKeySource::Environment("OPENAI_API_KEY".to_owned()));
    Ok(())
}

#[test]
fn an_openai_compatible_provider_with_no_key_is_refused_naming_both_fields() {
    let error = embedding_provider_config(&openai_compatible("", "")).err();

    assert_eq!(error.as_ref().map(SearchError::code), Some("embedding/config"));
    assert!(
        error.is_some_and(|error| error.to_string().contains("api_key or api_key_env")),
        "the refusal names both ways to supply the key"
    );
}
