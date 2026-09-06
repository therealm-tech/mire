import { describe, expect, it } from 'vitest'
import type { McpDescriptor } from './api'
import { activeServers, group, pick, serverNames } from './mcp'

function server(id: string, over: Partial<McpDescriptor> = {}): McpDescriptor {
  return {
    id,
    name: id,
    isDefault: true,
    url: `https://${id}`,
    tools: [],
    headers: [],
    usesAuth: [],
    ...over,
  }
}

/** One file, two stages, listed by id the way the registry lists them. */
const STAGED: McpDescriptor[] = [
  server('files@prod', { name: 'files', stage: 'prod', isDefault: false }),
  server('files@sandbox', { name: 'files', stage: 'sandbox', isDefault: true }),
]

describe('group', () => {
  it('puts the stages of one file on one entry', () => {
    const [entry, ...rest] = group([...STAGED, server('search')])
    expect(rest.map(({ name }) => name)).toEqual(['search'])
    expect(entry?.name).toBe('files')
    expect(entry?.entries.map(({ id }) => id)).toEqual(['files@prod', 'files@sandbox'])
  })

  it('falls back to the stage a bare name means, not to the first listed', () => {
    // `files@prod` sorts first and is not what `files` reaches. A panel opening
    // on `entries[0]` would open on the wrong endpoint.
    expect(group(STAGED)[0]?.fallback.id).toBe('files@sandbox')
  })
})

describe('pick', () => {
  it('takes the stage last picked', () => {
    const entry = group(STAGED)[0]
    expect(entry && pick(entry, { files: 'prod' }).id).toBe('files@prod')
  })

  it('falls back when the file no longer declares the remembered stage', () => {
    // `mcpStages` outlives a reload, and so outlives the stage it was about.
    const entry = group(STAGED)[0]
    expect(entry && pick(entry, { files: 'gone' }).id).toBe('files@sandbox')
  })
})

describe('activeServers', () => {
  it('takes one stage per file rather than every declaration of it', () => {
    expect(activeServers([...STAGED, server('search')], ['files', 'search'], {})).toEqual([
      'files@sandbox',
      'search',
    ])
    expect(activeServers(STAGED, ['files'], { files: 'prod' })).toEqual(['files@prod'])
  })

  it('reaches nothing until a server is switched on', () => {
    expect(activeServers(STAGED, [], { files: 'prod' })).toEqual([])
  })

  it('ignores a switched-on name nothing declares any more', () => {
    expect(activeServers([server('search')], ['deleted', 'search'], {})).toEqual(['search'])
  })
})

describe('serverNames', () => {
  it('names files, so a staged one is counted once', () => {
    expect(serverNames([...STAGED, server('search')])).toEqual(['files', 'search'])
  })
})
