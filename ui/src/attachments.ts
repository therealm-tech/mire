/**
 * Files on their way to a model endpoint, or to a server's own file store.
 *
 * Two destinations, and they are not variations of one thing:
 *
 * * **Inline** — the browser reads the file and the bytes travel in the body of
 *   the call, exactly like the text does. `mire` holds no session, so there is
 *   nothing to upload *to* and nothing that could expire between attaching a
 *   file and sending it, which is what keeps *Copy as curl* a reproduction
 *   rather than a reference to something only this tab had. The consequence is
 *   worth saying out loud: an attachment goes out again with every turn that
 *   follows it, because the whole history does. That is why the caps are small.
 * * **Uploaded** — the file goes to the `upload:` target an MCP server declares,
 *   out of band, and the turn carries the identifier it came back with. This is
 *   the only way a *tool* can be given a file at all: MCP has no primitive for
 *   handing a server bytes, and tool arguments are JSON in a context window,
 *   which a few megabytes of base64 is not. The model never sees the file — it
 *   sees fifteen characters it can quote into a tool call.
 *
 * Which of the two a file takes is the shape dropdown, like every other shape:
 * it is a decision, and a decision is exactly what this tool exists to let you
 * make rather than have made for you.
 */

import { z } from 'zod'
import type { Content, ContentPart, McpDescriptor, UploadExchange } from './api'
import { ApiError, uploadExchangeSchema, uploadFiles } from './api'
import { logger } from './logger'

/** How a file will be spelled on the wire. */
export type Shape = 'image' | 'file' | 'text' | 'upload'

/**
 * Largest single file that may be attached **inline**.
 *
 * Not a limit of the API — the server accepts far more — but of what is
 * pleasant to re-send on every subsequent turn and to read back in **Traffic**.
 * A screenshot or a short PDF fits; a video does not, and should not.
 */
export const MAX_FILE_BYTES = 4 * 1024 * 1024

/** Largest total that may ride along **inline** with one turn. */
export const MAX_TURN_BYTES = 8 * 1024 * 1024

/**
 * Largest file that may be uploaded.
 *
 * Far above the inline caps, because an upload does not ride in the body and
 * does not come back on the next turn — the turn carries an identifier. It
 * matches the ceiling on the upload route, so the composer is what refuses an
 * oversized file rather than a `413` from somewhere further along.
 *
 * What the *target* accepts is its own business, and deliberately not guessed at
 * here: a target refusing a file is an answer worth reading, not one to
 * pre-empt.
 */
export const MAX_UPLOAD_BYTES = 64 * 1024 * 1024

/** Where an uploaded file got to, or did not. */
export type UploadState =
  | { status: 'uploading' }
  /** It is there, and this is what it is called. */
  | { status: 'done'; server: string; fileId: string }
  | { status: 'failed'; message: string }

/** A file the composer is holding, read and ready to go. */
export interface Attachment {
  id: string
  /** Name as it was on disk. Several endpoints parse a file by its extension. */
  name: string
  /** What the browser called it, or an empty string when it had no idea. */
  mediaType: string
  /** Size in bytes, before base64 makes it a third larger. */
  size: number
  /** Which shape it goes out as. Chosen here, changeable there. */
  shape: Shape
  /**
   * The handle the browser gave us.
   *
   * Kept because a shape is changeable and an upload is lazy: switching a file
   * to *as an upload* has to be able to send bytes this module may never have
   * read, and re-picking the file to do it would be absurd.
   */
  file: File
  /**
   * `data:<mediaType>;base64,…` — what an `image_url` or a `file` part carries.
   *
   * Absent for a file too large to inline, which is also what stops the inline
   * shapes being offered for it: there is nothing to put in the body.
   */
  dataUrl?: string
  /**
   * The bytes decoded as UTF-8, when they decoded cleanly.
   *
   * Absent for anything binary, which is also what decides whether `text` is
   * offered as a shape at all: an endpoint cannot be handed a mangled PDF and
   * be expected to say something useful about it.
   */
  text?: string
  /** Where it got to, for a file whose shape is `upload`. */
  upload?: UploadState
}

let counter = 0
function nextId(): string {
  counter += 1
  return `attachment-${counter}`
}

/** Why a file was not attached. */
export interface Rejection {
  name: string
  reason: string
}

/** What reading a batch of files produced, and what it refused. */
export interface Attached {
  attachments: Attachment[]
  rejections: Rejection[]
}

/**
 * The server a file would be uploaded to, or `null` when there is none.
 *
 * The first of the profile's servers that declares an `upload:`. First rather
 * than a choice offered in the composer: a profile pointing at two servers with
 * two file stores is not a case anybody has, and inventing a second dropdown for
 * it would cost every ordinary run a decision.
 */
export function uploadServer(profileServers: string[], declared: McpDescriptor[]): string | null {
  const named = new Set(profileServers)
  return declared.find((server) => named.has(server.name) && server.upload)?.name ?? null
}

/**
 * The shape a file starts out as.
 *
 * A guess, and only a guess — it is a starting point the composer shows and lets
 * you change, because which shape an endpoint actually accepts is the sort of
 * thing this tool exists to find out rather than assume.
 *
 * An upload target changes the guess for binaries and nothing else. An image or
 * a text file is usually attached so the *model* can see it, and inline is how a
 * model sees anything; a binary the model cannot read is almost always meant for
 * the tools, which is exactly what an upload is for.
 */
export function defaultShape(
  mediaType: string,
  text: string | undefined,
  canUpload: boolean,
): Shape {
  if (mediaType.startsWith('image/')) {
    return 'image'
  }
  // Anything that decoded cleanly goes as text by default: every endpoint
  // understands a text part, and only some understand a `file` one.
  if (text !== undefined) {
    return 'text'
  }
  return canUpload ? 'upload' : 'file'
}

/** The shapes this file can be sent as. */
export function shapesFor(attachment: Attachment, canUpload: boolean): Shape[] {
  // Nothing was read, so there is nothing to inline: the file was too large for
  // the body and is only here at all because there is somewhere to put it.
  const inline: Shape[] =
    attachment.dataUrl === undefined
      ? []
      : attachment.text === undefined
        ? ['image', 'file']
        : ['image', 'file', 'text']
  return canUpload ? [...inline, 'upload'] : inline
}

/** What choosing a shape means, in a sentence, for the dropdown's title. */
export const SHAPE_LABELS: Record<Shape, string> = {
  image: 'as an image',
  file: 'as a file',
  text: 'as text',
  upload: 'as an upload',
}

/**
 * One attachment as the part it becomes.
 *
 * A text file goes out with its name above it. The name is not decoration: a
 * model asked about "the config" has no other way to know which of three
 * attached files that is, and an endpoint that only takes text parts is the
 * common case rather than the exotic one.
 *
 * An upload goes out as **text**, not as a `file` part carrying `file_id`, and
 * that is a deliberate reading of the shape: `file_id` means an identifier in
 * *the endpoint's own* file store, and this one lives in a store belonging to an
 * MCP server the endpoint has never heard of. Sending it there would be a
 * category error a strict gateway is entitled to answer `400` to. A sentence
 * naming the file and its id is understood by every endpoint, and it is what the
 * model has to repeat into a tool call anyway.
 */
export function attachmentPart(attachment: Attachment): ContentPart {
  switch (attachment.shape) {
    case 'image':
      return { type: 'image_url', image_url: { url: attachment.dataUrl ?? '' } }
    case 'file':
      return {
        type: 'file',
        file: { filename: attachment.name, file_data: attachment.dataUrl ?? '' },
      }
    case 'text':
      return {
        type: 'text',
        text: `--- ${attachment.name} ---\n${attachment.text ?? ''}`,
      }
    case 'upload':
      return { type: 'text', text: uploadedText(attachment) }
  }
}

/**
 * How an uploaded file is described to the model.
 *
 * Short and literal on purpose. The model's whole job here is to carry the id
 * into a tool call, so the id is quoted, named as an id, and sat next to the
 * filename it belongs to — the two facts a model needs to answer "which file do
 * you mean" and "what do I pass".
 */
function uploadedText(attachment: Attachment): string {
  const upload = attachment.upload
  if (upload?.status !== 'done') {
    // Unreachable from the composer, which will not send while an upload is
    // pending or failed. Said out loud rather than silently dropped: a turn that
    // quietly lost its file is the failure this whole path exists to avoid.
    return `--- ${attachment.name} --- was not uploaded, so no tool can read it.`
  }
  return (
    `--- ${attachment.name} --- uploaded to \`${upload.server}\`, ` +
    `file id \`${upload.fileId}\` (${humanSize(attachment.size)}). ` +
    `Pass that id to the tool that takes a file.`
  )
}

/**
 * The turn about to be sent.
 *
 * With nothing attached this is the bare string it has always been. That is not
 * a shortcut: an endpoint that has only ever been handed strings must keep
 * being handed strings, or every profile written before today would start
 * failing for a feature its author never used.
 */
export function composeContent(text: string, attachments: Attachment[]): Content {
  if (attachments.length === 0) {
    return text
  }
  const parts: ContentPart[] = text.length === 0 ? [] : [{ type: 'text', text }]
  return [...parts, ...attachments.map(attachmentPart)]
}

/**
 * Why this turn cannot go yet, or `null` when it can.
 *
 * An upload in flight or one that failed blocks **Send**, rather than the turn
 * going out with a file the tools cannot reach. The alternative is a run that
 * looks fine until a tool answers `404`, which is a long way from the composer
 * that could have said so.
 */
export function blocked(attachments: Attachment[]): string | null {
  const pending = attachments.filter(
    (attachment) => attachment.shape === 'upload' && attachment.upload?.status === 'uploading',
  )
  if (pending.length > 0) {
    return `${pending.length === 1 ? 'A file is' : `${pending.length} files are`} still uploading.`
  }
  const failed = attachments.find(
    (attachment) => attachment.shape === 'upload' && attachment.upload?.status !== 'done',
  )
  return failed === undefined ? null : `${failed.name} was not uploaded.`
}

/** Bytes, as a person reads them. */
export function humanSize(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`
  }
  if (bytes < 1024 * 1024) {
    return `${Math.round(bytes / 1024)} kB`
  }
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

/**
 * Reads what was dropped, picked or pasted.
 *
 * Refusals are returned rather than thrown: dropping four files of which one is
 * too large should attach three and say why the fourth did not, not lose the
 * lot. `carried` is what the turn is already holding, so the total cap counts
 * everything that will go out together — uploads excluded, because they do not
 * go out in the body at all.
 *
 * `canUpload` widens what is accepted rather than only what is offered: a file
 * over the inline cap is a rejection when there is nowhere to put it, and a
 * perfectly ordinary attachment when there is.
 */
export async function readFiles(
  files: File[],
  carried: Attachment[],
  canUpload: boolean,
): Promise<Attached> {
  const attachments: Attachment[] = []
  const rejections: Rejection[] = []
  let total = carried.filter(inlined).reduce((sum, attachment) => sum + attachment.size, 0)

  for (const file of files) {
    if (file.size > MAX_UPLOAD_BYTES) {
      rejections.push({
        name: file.name,
        reason: `${humanSize(file.size)} is over the ${humanSize(MAX_UPLOAD_BYTES)} limit`,
      })
      continue
    }

    // Too big for the body, but there is somewhere to put it: attached as an
    // upload and never read into memory here.
    if (file.size > MAX_FILE_BYTES) {
      if (!canUpload) {
        rejections.push({
          name: file.name,
          reason: `${humanSize(file.size)} is over the ${humanSize(MAX_FILE_BYTES)} limit for one file, and this profile has no server to upload it to`,
        })
        continue
      }
      attachments.push({
        id: nextId(),
        name: file.name,
        mediaType: file.type,
        size: file.size,
        shape: 'upload',
        file,
      })
      continue
    }

    if (total + file.size > MAX_TURN_BYTES && !canUpload) {
      rejections.push({
        name: file.name,
        reason: `this turn would carry more than ${humanSize(MAX_TURN_BYTES)}, and it carries it again on every turn after this one`,
      })
      continue
    }

    try {
      const attachment = await readFile(file, canUpload)
      attachments.push(attachment)
      if (inlined(attachment)) {
        total += file.size
      }
    } catch (error: unknown) {
      rejections.push({
        name: file.name,
        reason: error instanceof Error ? error.message : String(error),
      })
    }
  }

  logger.debug('attachments.read', {
    attached: attachments.length,
    rejected: rejections.length,
    inlineBytes: total,
  })
  return { attachments, rejections }
}

/** Whether this attachment's bytes ride in the body of the call. */
function inlined(attachment: Attachment): boolean {
  return attachment.shape !== 'upload'
}

async function readFile(file: File, canUpload: boolean): Promise<Attachment> {
  const [dataUrl, text] = await Promise.all([readAsDataUrl(file), readAsUtf8(file)])
  return {
    id: nextId(),
    name: file.name,
    mediaType: file.type,
    size: file.size,
    file,
    dataUrl,
    shape: defaultShape(file.type, text, canUpload),
    ...(text === undefined ? {} : { text }),
  }
}

/**
 * Sends one attachment to a server's upload target.
 *
 * Returns the *state* rather than a new attachment, deliberately: a few hundred
 * milliseconds pass while the bytes travel, and whoever attached the file is
 * free to rename nothing but change its shape, or detach it, in the meantime.
 * Handing back a whole attachment invites the caller to write it over the one
 * on screen, which would quietly undo whatever was done while it was in flight.
 *
 * The wire record comes back alongside, and is the point as much as the
 * identifier is: a `401` from the target belongs in **Traffic** next to
 * everything else this process said.
 *
 * Never throws. A failed upload is a state the chip shows and **Send** refuses
 * to go past, not an exception for a caller to invent a message for.
 */
export async function upload(
  attachments: Attachment[],
  server: string,
): Promise<{ states: Map<string, UploadState>; exchanges: UploadExchange[] }> {
  const states = new Map<string, UploadState>()
  try {
    const uploaded = await uploadFiles(
      server,
      attachments.map((attachment) => attachment.file),
    )
    logger.debug('attachments.uploaded', {
      server,
      files: uploaded.files.length,
      requests: uploaded.exchanges.length,
    })
    attachments.forEach((attachment, index) => {
      // Positional, which is the contract the API states: the nth identifier
      // belongs to the nth file. A file the target answered nothing for is a
      // failure of that file rather than of the batch.
      const fileId = uploaded.files[index]?.fileId
      states.set(
        attachment.id,
        fileId === undefined
          ? { status: 'failed', message: 'the target answered no identifier for this file' }
          : { status: 'done', server, fileId },
      )
    })
    return { states, exchanges: uploaded.exchanges }
  } catch (error: unknown) {
    const message = error instanceof Error ? error.message : String(error)
    logger.error('attachments.upload_failed', { server, files: attachments.length, message })
    for (const attachment of attachments) {
      states.set(attachment.id, { status: 'failed', message })
    }
    return { states, exchanges: refusedExchanges(error) }
  }
}

/**
 * The wire records a refused upload came back with.
 *
 * `mire` puts them in the error's `detail` for exactly this: a `413` from the
 * target is the whole explanation, and it belongs in **Traffic** rather than
 * only in a red line under the chip. Several of them, because a `raw` target
 * gets one request per file — the two that landed before the third did not are
 * the evidence that the failure is about a file rather than about the target.
 *
 * Validated like every other thing crossing this boundary: an error body is not
 * a reason to start trusting a shape.
 */
function refusedExchanges(error: unknown): UploadExchange[] {
  if (!(error instanceof ApiError)) {
    return []
  }
  const detail = error.body.detail
  if (detail === null || typeof detail !== 'object' || !('exchanges' in detail)) {
    return []
  }
  const parsed = z.array(uploadExchangeSchema).safeParse(detail.exchanges)
  return parsed.success ? parsed.data : []
}

/**
 * The bytes as a `data:` URL, which is the browser's own encoding of them.
 *
 * `FileReader` rather than a hand-rolled base64: encoding a few megabytes by
 * mapping over a byte array is how you blow the argument limit on
 * `String.fromCharCode`, and this is the path the platform already optimised.
 */
function readAsDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onerror = () =>
      reject(new Error(`could not be read (${reader.error?.name ?? 'unknown'})`))
    reader.onload = () =>
      typeof reader.result === 'string'
        ? resolve(reader.result)
        : reject(new Error('did not come back as a data URL'))
    reader.readAsDataURL(file)
  })
}

/**
 * The same bytes as text, when they are text.
 *
 * Decided by decoding rather than by a list of extensions: a `.log`, a `.tf` and
 * a file with no extension at all are all text, and a media type the browser
 * guessed from the name is not evidence. A replacement character means it was
 * not text, and `text` stops being an offered shape.
 */
function readAsUtf8(file: File): Promise<string | undefined> {
  return new Promise((resolve) => {
    const reader = new FileReader()
    reader.onerror = () => resolve(undefined)
    reader.onload = () => {
      const result = reader.result
      resolve(typeof result === 'string' && !result.includes('�') ? result : undefined)
    }
    reader.readAsText(file)
  })
}
