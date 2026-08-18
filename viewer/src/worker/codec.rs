use eframe::wasm_bindgen::{self};
use gloo_worker::Codec;

pub struct PreservingCodec;

/// A split index hash never fits a JS number: the directory's crc32 fills its upper half. Without
/// bigints it fails to encode, and the panic leaves the bridge's queue borrowed for good.
const SERIALIZER: serde_wasm_bindgen::Serializer =
    serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(true);

// This codec implementation relys on some internal implementation details about gloo worker message types.
// Fields marked with `#[serde(with = "serde_wasm_bindgen::preserve")]` will be passed as-is.
impl Codec for PreservingCodec {
    fn encode<I>(input: I) -> wasm_bindgen::JsValue
    where
        I: serde::Serialize,
    {
        input.serialize(&SERIALIZER).expect("failed to encode")
    }

    fn decode<O>(input: wasm_bindgen::JsValue) -> O
    where
        O: for<'de> serde::Deserialize<'de>,
    {
        serde_wasm_bindgen::from_value(input).expect("failed to decode")
    }
}
