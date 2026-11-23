// Attachment presentation adapted from T3 (MIT); see ../t3/provenance.json.
export type AttachmentFile = { id: string; name: string; size: number; type?: string; image?: boolean; deleted?: boolean; thread?: string };
export function uploadLimitError(files: readonly Pick<AttachmentFile, 'size'>[], attachedCount: number) {
  if (files.length + attachedCount > 4) return 'Up to four attachments per message.';
  if (files.some(file => file.size > 10 * 1024 * 1024)) return 'Maximum file size is 10 MiB.';
  return '';
}
