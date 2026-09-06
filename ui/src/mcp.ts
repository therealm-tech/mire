import type { McpDescriptor } from './api'

/**
 * The stages of one `mcp/` file, and the one a bare name means.
 *
 * `mire` declares a staged server once per stage — `weather@sandbox` and
 * `weather@acme` are two servers, with their own address, their own negotiated
 * revision and their own session. The panel still shows one card per *file*,
 * because two stages are two answers to the same question, and a run takes one
 * of them.
 */
export interface ServerGroup {
  name: string
  /** The entry a bare `name` reaches: the file's `default_stage:`. */
  fallback: McpDescriptor
  entries: McpDescriptor[]
}

/** Every declared server, grouped by the file that declared it. */
export function group(servers: McpDescriptor[]): ServerGroup[] {
  const groups = new Map<string, ServerGroup>()
  for (const server of servers) {
    const existing = groups.get(server.name)
    if (existing === undefined) {
      groups.set(server.name, { name: server.name, fallback: server, entries: [server] })
      continue
    }
    existing.entries.push(server)
    if (server.isDefault) {
      existing.fallback = server
    }
  }
  return [...groups.values()]
}

/**
 * The reading of one file a run would use: the stage last picked, else the one
 * a bare name means.
 *
 * A remembered stage the file no longer declares matches nothing and falls
 * through to the default, which is the same rule the model list follows.
 */
export function pick(entry: ServerGroup, stages: Record<string, string>): McpDescriptor {
  return entry.entries.find((server) => server.stage === stages[entry.name]) ?? entry.fallback
}

/**
 * The servers this run will actually set up, by id, in registry order.
 *
 * One id per file rather than per declaration: a staged file contributes the
 * stage that is picked and nothing else, so a run reaches `weather@acme` or
 * `weather@sandbox` and never both. `on` names files, not stages — switching a
 * server on is a statement about the server, and it survives changing which
 * stage of it you were asking.
 */
export function activeServers(
  servers: McpDescriptor[],
  on: string[],
  stages: Record<string, string>,
): string[] {
  return group(servers)
    .filter((entry) => on.includes(entry.name))
    .map((entry) => pick(entry, stages).id)
}
