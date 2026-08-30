use sift_protocol::CatalogDiagram;

use super::RequestState;

#[derive(Debug, Default)]
pub(super) struct CatalogDiagramState {
    diagram: Option<Box<CatalogDiagram>>,
    request: RequestState,
}

impl CatalogDiagramState {
    pub(super) fn diagram(&self) -> Option<&CatalogDiagram> {
        self.diagram.as_deref()
    }

    pub(super) fn request(&self) -> &RequestState {
        &self.request
    }

    pub(super) fn start_loading(&mut self) {
        self.request.start();
    }

    pub(super) fn finish_loading(&mut self, result: Result<Box<CatalogDiagram>, String>) {
        match result {
            Ok(diagram) => {
                self.diagram = Some(diagram);
                self.request.succeed();
            }
            Err(message) => {
                self.diagram = None;
                self.request.fail(message);
            }
        }
    }
}
