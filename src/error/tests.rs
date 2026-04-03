//! Tests for error types.

#[cfg(test)]
mod tests {
    use super::super::*;

    #[test]
    fn test_error_display() {
        let err = Error::config("invalid port");
        assert_eq!(err.to_string(), "configuration error: invalid port");
    }

    #[test]
    fn test_storage_error_not_found() {
        let err = StorageError::not_found("chunk", "123");
        assert_eq!(err.to_string(), "not found: chunk with id '123'");
    }

    #[test]
    fn test_storage_error_conversion() {
        let storage_err = StorageError::Database("connection failed".to_string());
        let err: Error = storage_err.into();
        assert!(matches!(err, Error::Storage(_)));
    }

    #[test]
    fn test_embedding_error_conversion() {
        let emb_err = EmbeddingError::ModelLoad("model.onnx not found".to_string());
        let err: Error = emb_err.into();
        assert!(matches!(err, Error::Embedding(_)));
    }

    #[test]
    fn test_watcher_error_conversion() {
        let watch_err = WatcherError::WatchFailed {
            path: "/tmp/test".to_string(),
            reason: "permission denied".to_string(),
        };
        let err: Error = watch_err.into();
        assert!(matches!(err, Error::Watcher(_)));
    }

    #[test]
    fn test_server_error_conversion() {
        let server_err = ServerError::BindFailed {
            address: "127.0.0.1:8080".to_string(),
            reason: "address in use".to_string(),
        };
        let err: Error = server_err.into();
        assert!(matches!(err, Error::Server(_)));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
    }

    #[test]
    fn test_result_type_alias() {
        fn returns_ok() -> Result<i32> {
            Ok(42)
        }

        fn returns_err() -> Result<i32> {
            Err(Error::config("test error"))
        }

        assert!(returns_ok().is_ok());
        assert!(returns_err().is_err());
    }

    #[test]
    fn test_error_debug_format() {
        let err = Error::Internal("something went wrong".to_string());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("Internal"));
        assert!(debug_str.contains("something went wrong"));
    }

    #[test]
    fn test_error_internal() {
        let err = Error::internal("test internal error");
        assert_eq!(err.to_string(), "internal error: test internal error");
    }

    #[test]
    fn test_storage_error_database() {
        let err = StorageError::Database("connection timeout".to_string());
        assert_eq!(err.to_string(), "database error: connection timeout");
    }

    #[test]
    fn test_storage_error_migration() {
        let err = StorageError::Migration("migration 001 failed".to_string());
        assert_eq!(err.to_string(), "migration error: migration 001 failed");
    }

    #[test]
    fn test_storage_error_vector() {
        let err = StorageError::Vector("invalid vector dimension".to_string());
        assert_eq!(err.to_string(), "vector error: invalid vector dimension");
    }

    #[test]
    fn test_embedding_error_runtime() {
        let err = EmbeddingError::Runtime("ONNX session failed".to_string());
        assert_eq!(err.to_string(), "ONNX runtime error: ONNX session failed");
    }

    #[test]
    fn test_embedding_error_tokenization() {
        let err = EmbeddingError::Tokenization("invalid token".to_string());
        assert_eq!(err.to_string(), "tokenization error: invalid token");
    }

    #[test]
    fn test_embedding_error_worker_pool() {
        let err = EmbeddingError::WorkerPool("worker thread panicked".to_string());
        assert_eq!(err.to_string(), "worker pool error: worker thread panicked");
    }

    #[test]
    fn test_watcher_error_process_failed() {
        let err = WatcherError::ProcessFailed {
            path: "/src/main.rs".to_string(),
            reason: "parse error".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "failed to process file '/src/main.rs': parse error"
        );
    }

    #[test]
    fn test_watcher_error_indexing() {
        let err = WatcherError::Indexing("index out of bounds".to_string());
        assert_eq!(err.to_string(), "indexing error: index out of bounds");
    }

    #[test]
    fn test_server_error_request() {
        let err = ServerError::Request("malformed request body".to_string());
        assert_eq!(err.to_string(), "request error: malformed request body");
    }

    #[test]
    fn test_server_error_mcp() {
        let err = ServerError::Mcp("invalid MCP message".to_string());
        assert_eq!(err.to_string(), "MCP error: invalid MCP message");
    }

    #[test]
    fn test_error_debug_internal() {
        let err = Error::Internal("debug test".to_string());
        let debug_str = format!("{err:?}");
        assert!(debug_str.contains("Internal"));
    }

    #[test]
    fn test_chained_error_conversion() {
        let storage_err = StorageError::Database("db failed".to_string());
        let main_err = Error::from(storage_err);
        let result: Result<()> = Err(main_err);
        assert!(result.is_err());
    }

    #[test]
    fn test_multiple_error_types_in_result() {
        fn might_fail_storage() -> Result<String> {
            Err(Error::Storage(StorageError::Database("test".to_string())))
        }

        fn might_fail_embedding() -> Result<String> {
            Err(Error::Embedding(EmbeddingError::ModelLoad(
                "test".to_string(),
            )))
        }

        assert!(might_fail_storage().is_err());
        assert!(might_fail_embedding().is_err());
    }

    #[test]
    fn test_storage_not_found_with_numbers() {
        let err = StorageError::not_found("lesson", "42");
        assert_eq!(err.to_string(), "not found: lesson with id '42'");
    }

    #[test]
    fn test_error_propagation_with_question_mark() {
        fn inner() -> Result<i32> {
            Err(Error::config("inner error"))
        }

        fn outer() -> Result<i32> {
            let _ = inner()?;
            Ok(0)
        }

        let result = outer();
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().to_string(),
            "configuration error: inner error"
        );
    }
}
