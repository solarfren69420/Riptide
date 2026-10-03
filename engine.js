// Published builds carry a gzip file so static hosts need no compression settings.
// Local build-web.sh output can still use wasm-bindgen's ordinary .wasm loader.
export async function loadEngine(init) {
  const response = await fetch(new URL("./pkg/riptide_bg.wasm.gz", import.meta.url));
  if (response.status === 404) return init();
  if (!response.ok) throw new Error(`Engine download failed (${response.status}). Please retry.`);

  let bytes = new Uint8Array(await response.arrayBuffer());
  // Some hosts may already decode gzip through Content-Encoding.
  if (bytes[0] === 0x1f && bytes[1] === 0x8b) {
    if (typeof DecompressionStream === "undefined") {
      throw new Error("This browser cannot decompress the engine. Please update your browser.");
    }
    const stream = new Blob([bytes]).stream().pipeThrough(new DecompressionStream("gzip"));
    bytes = new Uint8Array(await new Response(stream).arrayBuffer());
  }
  return init({ module_or_path: bytes });
}
