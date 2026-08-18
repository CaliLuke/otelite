// gRPC TraceService implementation for OTLP

use crate::signals::TracesHandler;
use opentelemetry_proto::tonic::collector::trace::v1::{
    trace_service_server::{TraceService, TraceServiceServer},
    ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use std::sync::Arc;
use tonic::{Request, Response, Status};
use tracing::{debug, error};

/// Implementation of the OTLP TraceService
pub struct TraceServiceImpl {
    handler: Arc<TracesHandler>,
}

impl TraceServiceImpl {
    /// Create a new TraceService implementation
    pub fn new(handler: Arc<TracesHandler>) -> Self {
        Self { handler }
    }

    /// Convert into a tonic service
    pub fn into_service(self) -> TraceServiceServer<Self> {
        TraceServiceServer::new(self)
    }
}

#[tonic::async_trait]
impl TraceService for TraceServiceImpl {
    async fn export(
        &self,
        request: Request<ExportTraceServiceRequest>,
    ) -> Result<Response<ExportTraceServiceResponse>, Status> {
        debug!("Received trace export request");

        let req = request.into_inner();

        // Process the traces through the handler
        match self.handler.process(req).await {
            Ok(result) => {
                debug!(
                    accepted_spans = result.accepted_spans,
                    rejected_spans = result.rejected_spans,
                    "Processed trace export request"
                );
                Ok(Response::new(ExportTraceServiceResponse {
                    partial_success: (result.rejected_spans > 0).then_some(
                        ExportTracePartialSuccess {
                            rejected_spans: i64::try_from(result.rejected_spans)
                                .unwrap_or(i64::MAX),
                            error_message:
                                "Otelite rejected spans with invalid trace or span identifiers"
                                    .to_string(),
                        },
                    ),
                }))
            },
            Err(e) => {
                error!("Failed to process traces: {}", e);
                Err(e.to_grpc_status())
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span as OtlpSpan};
    use otelite_storage::{sqlite::SqliteBackend, StorageBackend, StorageConfig};

    #[tokio::test]
    async fn test_trace_service_creation() {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let config = StorageConfig {
            data_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let mut storage = SqliteBackend::new(config);
        storage
            .initialize()
            .await
            .expect("Failed to initialize storage");
        let handler = Arc::new(TracesHandler::new(Arc::new(storage)));
        let _service = TraceServiceImpl::new(handler);
    }

    #[tokio::test]
    async fn test_trace_export_empty() {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let config = StorageConfig {
            data_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let mut storage = SqliteBackend::new(config);
        storage
            .initialize()
            .await
            .expect("Failed to initialize storage");
        let handler = Arc::new(TracesHandler::new(Arc::new(storage)));
        let service = TraceServiceImpl::new(handler);

        let request = Request::new(ExportTraceServiceRequest {
            resource_spans: vec![],
        });

        let response = service.export(request).await;
        assert!(response.is_ok());
    }

    #[tokio::test]
    async fn test_trace_export_reports_rejected_invalid_spans() {
        let temp_dir = tempfile::tempdir().expect("Failed to create temp dir");
        let config = StorageConfig {
            data_dir: temp_dir.path().to_path_buf(),
            ..Default::default()
        };
        let mut storage = SqliteBackend::new(config);
        storage
            .initialize()
            .await
            .expect("Failed to initialize storage");
        let handler = Arc::new(TracesHandler::new(Arc::new(storage)));
        let service = TraceServiceImpl::new(handler);
        let request = Request::new(ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                resource: None,
                scope_spans: vec![ScopeSpans {
                    scope: None,
                    spans: vec![OtlpSpan {
                        trace_id: vec![0; 16],
                        span_id: vec![1; 8],
                        ..Default::default()
                    }],
                    schema_url: String::new(),
                }],
                schema_url: String::new(),
            }],
        });

        let response = service
            .export(request)
            .await
            .expect("request succeeds with partial rejection")
            .into_inner();

        assert_eq!(
            response
                .partial_success
                .expect("partial success")
                .rejected_spans,
            1
        );
    }
}
