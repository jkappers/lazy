//! Docs (D30): the requests behind `DocsEffect::Request`.
//!
//! Reads are repeated after an unknown outcome, like other reads. A mutation is a single attempt:
//! the core sends it again, with the same operation id, when the outcome is unknown, and the
//! server answers the repeat with the original result. Nothing here is cached on disk; the
//! journal of creates and saves lives in the state store.

use std::sync::Arc;
use std::time::Duration;

use lazy_client_core::{DocsAnswer, DocsCall, FileSource, Input};
use lazy_protocol::doc_paths;

use super::{Inner, RequestError};

/// Uploads of a page's file may take as long as an attachment's.
const FILE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// Largest page file read whole into memory (the server's own limit is the same).
const MAX_PAGE_FILE_BYTES: u64 = 25 * 1024 * 1024;

impl Inner {
    pub(super) fn docs_request(self: &Arc<Self>, id: u64, call: DocsCall) {
        let inner = self.clone();
        self.spawn(async move {
            let result = inner.run_docs_call(call).await;
            inner.emit(Input::DocsReply { id, result }).await;
        });
    }

    async fn run_docs_call(&self, call: DocsCall) -> Result<DocsAnswer, RequestError> {
        let api = &self.api;
        match call {
            DocsCall::ListPages {
                conversation,
                params,
            } => {
                let path = doc_paths::conversation_pages(conversation);
                self.retry_read(|| api.get_query(&path, &params))
                    .await
                    .map(DocsAnswer::Pages)
            }
            DocsCall::ListWorkspacePages { workspace, params } => {
                let path = doc_paths::workspace_pages(workspace);
                // A search: like message search, a refusal for time is not repeated unchanged.
                if params.q.is_some() {
                    api.search(&path, &params).await.map(DocsAnswer::Pages)
                } else {
                    self.retry_read(|| api.get_query(&path, &params))
                        .await
                        .map(DocsAnswer::Pages)
                }
            }
            DocsCall::GetPage(page) => {
                let path = doc_paths::page(page);
                self.retry_read(|| api.get(&path))
                    .await
                    .map(DocsAnswer::Page)
            }
            DocsCall::CreatePage {
                conversation,
                request,
            } => api
                .post(&doc_paths::conversation_pages(conversation), &request)
                .await
                .map(DocsAnswer::Page),
            DocsCall::UpdatePage { page, request } => api
                .patch(&doc_paths::page(page), &request)
                .await
                .map(DocsAnswer::Page),
            DocsCall::TrashPage { page, request } => api
                .delete_json(&doc_paths::page(page), &request)
                .await
                .map(DocsAnswer::Page),
            DocsCall::RestorePage { page, request } => api
                .post(&doc_paths::page_restore(page), &request)
                .await
                .map(DocsAnswer::Page),
            DocsCall::PurgePage { page, request } => api
                .post_json_unit(&doc_paths::page_purge(page), &request)
                .await
                .map(|()| DocsAnswer::Done),
            DocsCall::ListVersions(page) => {
                let path = doc_paths::page_versions(page);
                self.retry_read(|| api.get(&path))
                    .await
                    .map(DocsAnswer::Versions)
            }
            DocsCall::GetVersion { page, version } => {
                let path = doc_paths::page_version(page, version);
                self.retry_read(|| api.get(&path))
                    .await
                    .map(DocsAnswer::Version)
            }
            DocsCall::RestoreVersion {
                page,
                version,
                request,
            } => api
                .post(&doc_paths::page_version_restore(page, version), &request)
                .await
                .map(DocsAnswer::Page),
            DocsCall::ListNotations(page) => {
                let path = doc_paths::page_notations(page);
                self.retry_read(|| api.get(&path))
                    .await
                    .map(DocsAnswer::Notations)
            }
            DocsCall::CreateNotation { page, request } => api
                .post(&doc_paths::page_notations(page), &request)
                .await
                .map(DocsAnswer::Notation),
            DocsCall::UpdateNotation { notation, request } => api
                .patch(&doc_paths::notation(notation), &request)
                .await
                .map(DocsAnswer::Notation),
            DocsCall::SetNotationCompleted { notation, request } => api
                .put(&doc_paths::notation_completed(notation), &request)
                .await
                .map(DocsAnswer::Notation),
            DocsCall::DeleteNotation { notation, request } => api
                .delete_json_unit(&doc_paths::notation(notation), &request)
                .await
                .map(|()| DocsAnswer::Done),
            DocsCall::ReplyToNotation { notation, request } => api
                .post(&doc_paths::notation_replies(notation), &request)
                .await
                .map(DocsAnswer::Notation),
            DocsCall::DeleteNotationReply { reply, request } => api
                .delete_json(&doc_paths::notation_reply(reply), &request)
                .await
                .map(DocsAnswer::Notation),
            DocsCall::ListWorkspaceNotations { workspace, params } => {
                let path = doc_paths::workspace_notations(workspace);
                self.retry_read(|| api.get_query(&path, &params))
                    .await
                    .map(DocsAnswer::Notations)
            }
            DocsCall::UploadPageFile {
                page,
                attachment,
                source,
            } => {
                let (name, bytes) = read_source(source).await?;
                let media_type = lazy_domain::attachment::resolve_media_type(
                    lazy_client_core::guess_media_type(&name),
                    &bytes,
                );
                let length = bytes.len() as u64;
                api.put_attachment(
                    &doc_paths::page_file(page, attachment),
                    &name,
                    &media_type,
                    length,
                    reqwest::Body::from(bytes),
                    FILE_TIMEOUT,
                )
                .await
                .map(|response| DocsAnswer::File(response.attachment))
            }
        }
    }
}

/// The name and bytes of a chosen file. A failure to read is reported as a refusal with a plain
/// reason (nothing was sent), never as an unknown outcome.
async fn read_source(source: FileSource) -> Result<(String, Vec<u8>), RequestError> {
    let refused = |message: String| {
        RequestError::Rejected(lazy_protocol::ApiError::new(
            lazy_protocol::ErrorCode::InvalidRequest,
            message,
        ))
    };
    match source {
        FileSource::Bytes(bytes) => {
            if bytes.len() as u64 > MAX_PAGE_FILE_BYTES {
                return Err(refused("That file is too large to add to a page.".into()));
            }
            Ok(("Pasted file".to_owned(), bytes.as_slice().to_vec()))
        }
        FileSource::Path(path) => {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_owned();
            let read = tokio::task::spawn_blocking(move || {
                let length = std::fs::metadata(&path)?.len();
                if length > MAX_PAGE_FILE_BYTES {
                    return Ok(None);
                }
                std::fs::read(&path).map(Some)
            })
            .await
            .map_err(|_| refused("The file could not be read.".into()))?;
            match read {
                Ok(Some(bytes)) => Ok((name, bytes)),
                Ok(None) => Err(refused("That file is too large to add to a page.".into())),
                Err(_) => Err(refused("The file could not be read.".into())),
            }
        }
    }
}
