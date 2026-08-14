import { describe, expect, it } from 'vitest'
import type { McpDescriptor } from './api'
import {
  type Attachment,
  attachmentPart,
  blocked,
  composeContent,
  defaultShape,
  humanSize,
  MAX_FILE_BYTES,
  MAX_UPLOAD_BYTES,
  readFiles,
  shapesFor,
  uploadServer,
} from './attachments'

function attachment(over: Partial<Attachment> = {}): Attachment {
  return {
    id: 'attachment-1',
    name: 'notes.txt',
    mediaType: 'text/plain',
    size: 5,
    shape: 'text',
    file: new File(['hello'], 'notes.txt', { type: 'text/plain' }),
    dataUrl: 'data:text/plain;base64,aGVsbG8=',
    text: 'hello',
    ...over,
  }
}

/** An upload target, as `GET /api/mcp` describes one. */
function target(): NonNullable<McpDescriptor['upload']> {
  return {
    url: 'https://library.internal/v1/documents',
    method: 'POST',
    body: 'multipart',
    id: ['$.id'],
  }
}

function server(over: Partial<McpDescriptor> = {}): McpDescriptor {
  return {
    name: 'weather',
    url: 'https://mcp.internal/mcp',
    tools: [],
    headers: [],
    usesAuth: [],
    ...over,
  }
}

describe('composeContent', () => {
  it('leaves a turn with nothing attached as the bare string it always was', () => {
    expect(composeContent('ping', [])).toBe('ping')
  })

  it('puts the question first and the files after it', () => {
    const content = composeContent('what is in this?', [
      attachment({ shape: 'image', name: 'shot.png', dataUrl: 'data:image/png;base64,iVBOR' }),
    ])

    expect(content).toEqual([
      { type: 'text', text: 'what is in this?' },
      { type: 'image_url', image_url: { url: 'data:image/png;base64,iVBOR' } },
    ])
  })

  it('sends a file with no question at all, because showing something is asking', () => {
    const content = composeContent('', [attachment({ shape: 'file' })])

    expect(Array.isArray(content) && content).toHaveLength(1)
    expect(content[0]).toEqual({
      type: 'file',
      file: { filename: 'notes.txt', file_data: 'data:text/plain;base64,aGVsbG8=' },
    })
  })
})

describe('attachmentPart', () => {
  it('names a text file above its own content, so the model can tell three apart', () => {
    expect(attachmentPart(attachment())).toEqual({
      type: 'text',
      text: '--- notes.txt ---\nhello',
    })
  })

  it('spells an image the way an endpoint reads it', () => {
    expect(
      attachmentPart(attachment({ shape: 'image', dataUrl: 'data:image/png;base64,x' })),
    ).toEqual({ type: 'image_url', image_url: { url: 'data:image/png;base64,x' } })
  })

  /**
   * The whole point of the upload shape: the bytes are somewhere else, and what
   * the turn carries is the fifteen characters a model has to quote into a tool
   * call. Text rather than a `file_id` part, because `file_id` means an id in
   * *the endpoint's* store and this one is in an MCP server's.
   */
  it('sends an uploaded file as its identifier, not as its bytes', () => {
    const part = attachmentPart(
      attachment({
        name: 'deck.pptx',
        shape: 'upload',
        size: 1_468_006,
        upload: { status: 'done', server: 'library', fileId: 'doc_7f3a' },
      }),
    )

    expect(part.type).toBe('text')
    const text = part.type === 'text' ? part.text : ''
    expect(text).toContain('deck.pptx')
    expect(text).toContain('doc_7f3a')
    expect(text).toContain('library')
    // Not a byte of it goes into the context window.
    expect(text).not.toContain('data:')
  })
})

describe('defaultShape', () => {
  it('starts an image as an image', () => {
    expect(defaultShape('image/png', undefined, false)).toBe('image')
    // Even where there is somewhere to upload it: an image is attached so the
    // model can see it, and inline is how a model sees anything.
    expect(defaultShape('image/png', undefined, true)).toBe('image')
  })

  it('starts anything that decoded cleanly as text, which every endpoint takes', () => {
    expect(defaultShape('application/json', '{"a":1}', false)).toBe('text')
    expect(defaultShape('', 'no media type, still text', true)).toBe('text')
  })

  it('starts anything binary as a file, or as an upload when there is one', () => {
    expect(defaultShape('application/pdf', undefined, false)).toBe('file')
    expect(defaultShape('application/pdf', undefined, true)).toBe('upload')
  })
})

describe('shapesFor', () => {
  it('does not offer to send a PDF as text it could not decode', () => {
    const binary: Attachment = attachment({ shape: 'file' })
    delete binary.text
    expect(shapesFor(binary, false)).toEqual(['image', 'file'])
  })

  it('offers all three for something that did decode', () => {
    expect(shapesFor(attachment(), false)).toEqual(['image', 'file', 'text'])
  })

  it('adds the upload only where a server declares one', () => {
    expect(shapesFor(attachment(), true)).toEqual(['image', 'file', 'text', 'upload'])
  })

  it('offers nothing but the upload for a file that was never read', () => {
    const huge: Attachment = attachment({ shape: 'upload', size: MAX_FILE_BYTES * 2 })
    delete huge.dataUrl
    delete huge.text
    expect(shapesFor(huge, true)).toEqual(['upload'])
  })
})

describe('uploadServer', () => {
  it('finds the profile server that has somewhere to put a file', () => {
    const declared = [server(), server({ name: 'library', upload: target() })]
    expect(uploadServer(['weather', 'library'], declared)).toBe('library')
  })

  /**
   * A target on a server this run never talks to is a store whose identifiers no
   * tool in this run can resolve, so it is not one.
   */
  it('ignores a target on a server the profile does not name', () => {
    const declared = [server({ name: 'library', upload: target() })]
    expect(uploadServer(['weather'], declared)).toBeNull()
  })

  it('is null when nothing declares one', () => {
    expect(uploadServer(['weather'], [server()])).toBeNull()
  })
})

describe('blocked', () => {
  it('lets an ordinary turn through', () => {
    expect(blocked([attachment()])).toBeNull()
  })

  it('holds a turn whose file is still on its way', () => {
    const waiting = attachment({ shape: 'upload', upload: { status: 'uploading' } })
    expect(blocked([waiting])).toContain('uploading')
  })

  /**
   * The alternative is a turn quoting an id that resolves to nothing, and a run
   * that fails three requests later with nothing on screen connecting the two.
   */
  it('holds a turn whose file never arrived', () => {
    const failed = attachment({
      name: 'deck.pptx',
      shape: 'upload',
      upload: { status: 'failed', message: '413' },
    })
    expect(blocked([failed])).toContain('deck.pptx')
  })
})

describe('readFiles', () => {
  it('reads a file into a data URL and its text at once', async () => {
    const read = await readFiles(
      [new File(['hello'], 'notes.txt', { type: 'text/plain' })],
      [],
      false,
    )

    expect(read.rejections).toEqual([])
    expect(read.attachments).toHaveLength(1)
    expect(read.attachments[0]?.name).toBe('notes.txt')
    expect(read.attachments[0]?.dataUrl).toMatch(/^data:text\/plain;base64,/)
    expect(read.attachments[0]?.text).toBe('hello')
    expect(read.attachments[0]?.shape).toBe('text')
  })

  /**
   * Attachments ride along on every later turn, so a cap that only refused the
   * whole batch would be the wrong trade: three good files should not be lost
   * to one oversized one.
   */
  it('attaches what fits and says why the rest did not', async () => {
    const big = new File(['x'.repeat(MAX_FILE_BYTES + 1)], 'huge.bin')
    const small = new File(['ok'], 'small.txt', { type: 'text/plain' })

    const read = await readFiles([big, small], [], false)

    expect(read.attachments.map((entry) => entry.name)).toEqual(['small.txt'])
    expect(read.rejections).toHaveLength(1)
    expect(read.rejections[0]?.name).toBe('huge.bin')
    expect(read.rejections[0]?.reason).toContain('over the')
  })

  /**
   * The inline cap is about what a body can carry on every turn. An upload
   * carries an identifier, so the same file stops being too large the moment
   * there is somewhere to put it.
   */
  it('takes a file over the inline cap when there is somewhere to upload it', async () => {
    const big = new File(['x'.repeat(MAX_FILE_BYTES + 1)], 'deck.pptx')

    const read = await readFiles([big], [], true)

    expect(read.rejections).toEqual([])
    expect(read.attachments[0]?.shape).toBe('upload')
    // Never read into memory: there is nothing to put in a body.
    expect(read.attachments[0]?.dataUrl).toBeUndefined()
  })

  it('refuses what is over the upload ceiling too', async () => {
    const huge = new File(['x'.repeat(MAX_UPLOAD_BYTES + 1)], 'video.mov')

    const read = await readFiles([huge], [], true)

    expect(read.attachments).toEqual([])
    expect(read.rejections[0]?.reason).toContain('over the')
  })

  it('counts what the turn is already carrying towards the total', async () => {
    const carried: Attachment[] = [attachment({ size: MAX_FILE_BYTES * 2 })]
    const read = await readFiles(
      [new File(['x'.repeat(MAX_FILE_BYTES)], 'another.bin')],
      carried,
      false,
    )

    expect(read.attachments).toEqual([])
    expect(read.rejections[0]?.reason).toContain('every turn after this one')
  })

  /** An upload is not in the body, so it cannot make the body too large. */
  it('does not count an upload towards the turn total', async () => {
    const carried: Attachment[] = [
      attachment({ shape: 'upload', size: MAX_FILE_BYTES * 2, upload: { status: 'uploading' } }),
    ]
    const read = await readFiles(
      [new File(['ok'], 'small.txt', { type: 'text/plain' })],
      carried,
      true,
    )

    expect(read.rejections).toEqual([])
    expect(read.attachments).toHaveLength(1)
  })
})

describe('humanSize', () => {
  it('reads like a size rather than a byte count', () => {
    expect(humanSize(512)).toBe('512 B')
    expect(humanSize(2048)).toBe('2 kB')
    expect(humanSize(4 * 1024 * 1024)).toBe('4.0 MB')
  })
})
