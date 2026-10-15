// Normalize user-selected bitmaps before they cross native-message limits.
// Image bytes live only in the draft until saved by the conversation service.
export async function prepareImage(file) {
  if (!['image/png','image/jpeg','image/webp'].includes(file.type)) throw new Error('Use PNG, JPEG or WebP images.');
  if (file.size > 20 * 1024 * 1024) throw new Error('The source image exceeds 20 MiB.');
  const bitmap = await createImageBitmap(file);
  try {
    const scale = Math.min(1, 2000 / Math.max(bitmap.width, bitmap.height));
    const canvas = new OffscreenCanvas(Math.max(1, Math.round(bitmap.width * scale)), Math.max(1, Math.round(bitmap.height * scale)));
    const context = canvas.getContext('2d');
    if (!context) throw new Error('Image conversion is unavailable.');
    context.drawImage(bitmap, 0, 0, canvas.width, canvas.height);
    let blob = await canvas.convertToBlob({ type: 'image/png' });
    if (blob.size > 512 * 1024) {
      // White background preserves transparency legibility in JPEG.
      context.globalCompositeOperation = 'destination-over'; context.fillStyle = '#ffffff'; context.fillRect(0, 0, canvas.width, canvas.height);
      for (const quality of [0.85, 0.7, 0.5, 0.3]) {
        blob = await canvas.convertToBlob({ type: 'image/jpeg', quality });
        if (blob.size <= 512 * 1024) break;
      }
    }
    if (blob.size > 512 * 1024) throw new Error('This image is still too large; crop or resize it before attaching.');
    const bytes = new Uint8Array(await blob.arrayBuffer());
    let binary = ''; for (const byte of bytes) binary += String.fromCharCode(byte);
    return { data: btoa(binary), mediaType: blob.type, name: file.name || 'Image' };
  } finally { bitmap.close(); }
}
