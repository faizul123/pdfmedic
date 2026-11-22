//! Stage 7: document metadata.

use pipeline_core::{Document, PdfDoc, Result, Stage};

pub struct MetadataStage;

impl Stage for MetadataStage {
    fn name(&self) -> &'static str {
        "metadata"
    }

    fn run_document(&self, doc: &dyn PdfDoc, model: &mut Document) -> Result<()> {
        model.metadata = doc.metadata()?;
        Ok(())
    }
}
