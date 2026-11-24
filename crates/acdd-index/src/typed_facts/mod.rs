pub mod binary;
pub(crate) mod bundle;
mod catalog;
pub(crate) mod flow_store;
pub(crate) mod node_view;
mod presentation;
pub(crate) mod semantic_store;

pub use crate::semantic::types::{encoded_scope_fact_count, scope_fact_count};
pub use bundle::TypedFacts;
pub use catalog::SymbolCatalog;
pub(crate) use catalog::{decode_catalog_references, CatalogStoragePayload};
pub use flow_store::{FlowState, FlowStore};
pub use semantic_store::SemanticState;
#[doc(hidden)]
pub use crate::models::semantic::NodeSemanticEntry;

pub(crate) const LOCAL_FACTS_BINARY_MAGIC: &[u8] = b"ACDD-LOCAL-FACTS\0\x06";

pub(crate) fn compressed_facts_payload(blob: &[u8]) -> Option<&[u8]> {
    blob.strip_prefix(LOCAL_FACTS_BINARY_MAGIC)
}

thread_local! {
    static LOCAL_FACTS_BUFFER: std::cell::RefCell<Vec<u8>> = const { std::cell::RefCell::new(Vec::new()) };
    static LOCAL_FACTS_COMPRESSOR: std::cell::RefCell<Option<zstd::bulk::Compressor<'static>>> =
        const { std::cell::RefCell::new(None) };
    static LOCAL_FACTS_DECODER: std::cell::RefCell<Option<zstd::zstd_safe::DCtx<'static>>> =
        const { std::cell::RefCell::new(None) };
}

pub fn encode_facts(facts: &TypedFacts) -> anyhow::Result<Vec<u8>> {
    encode_facts_with_len(facts).map(|(encoded, _)| encoded)
}

/// Encodes facts and returns their uncompressed binary size.
pub fn encode_facts_with_len(facts: &TypedFacts) -> anyhow::Result<(Vec<u8>, u64)> {
    anyhow::ensure!(
        !facts.flows.has_invalid_named_uncertainty_carriers(),
        "malformed named-definition uncertainty carrier"
    );
    LOCAL_FACTS_BUFFER.with(|buffer| -> anyhow::Result<(Vec<u8>, u64)> {
        let mut raw = buffer.borrow_mut();
        raw.clear();
        facts.encode_with_string_table(&mut raw);
        LOCAL_FACTS_COMPRESSOR.with(|slot| -> anyhow::Result<(Vec<u8>, u64)> {
            let mut slot = slot.borrow_mut();
            if slot.is_none() {
                *slot = Some(zstd::bulk::Compressor::new(1)?);
            }
            let compressor = slot.as_mut().expect("facts compressor initialized");
            let mut encoded = compressor.compress(&raw)?;
            encoded.reserve(LOCAL_FACTS_BINARY_MAGIC.len());
            encoded.splice(0..0, LOCAL_FACTS_BINARY_MAGIC.iter().copied());
            Ok((encoded, u64::try_from(raw.len())?))
        })
    })
}

pub fn decode_facts(blob: &[u8]) -> anyhow::Result<TypedFacts> {
    let compressed = compressed_facts_payload(blob)
        .ok_or_else(|| anyhow::anyhow!("unsupported local facts format"))?;
    let facts = LOCAL_FACTS_DECODER.with(|slot| -> anyhow::Result<TypedFacts> {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(zstd::zstd_safe::DCtx::try_create().ok_or_else(|| {
                std::io::Error::other("failed to allocate facts decompression context")
            })?);
        }
        let context = slot.as_mut().expect("facts decoder initialized");
        context
            .init()
            .map_err(|code| std::io::Error::other(zstd::zstd_safe::get_error_name(code)))?;
        LOCAL_FACTS_BUFFER.with(|buffer| -> anyhow::Result<TypedFacts> {
            let mut raw = buffer.borrow_mut();
            raw.clear();
            let mut decoder = zstd::stream::read::Decoder::with_context(compressed, context);
            std::io::Read::read_to_end(&mut decoder, &mut raw)?;
            let mut cursor = 0;
            let facts = TypedFacts::decode_with_string_table(&raw, &mut cursor)
                .ok_or_else(|| anyhow::anyhow!("malformed binary local facts"))?;
            anyhow::ensure!(cursor == raw.len(), "trailing bytes in binary local facts");
            Ok(facts)
        })
    })?;
    anyhow::ensure!(
        !facts.flows.has_invalid_named_uncertainty_carriers(),
        "malformed named-definition uncertainty carrier"
    );
    Ok(facts)
}
