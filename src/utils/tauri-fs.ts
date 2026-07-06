/**
 * Typed dynamic loader for `@tauri-apps/plugin-fs`.
 *
 * `plugin-fs` is an optional Tauri plugin that is not (yet) declared as a
 * hard dependency in package.json. We load it lazily at runtime only inside
 * `isTauri` code paths so that:
 *   - `vue-tsc --noEmit` does not error on a missing module, and
 *   - the browser build never attempts to import it.
 *
 * If the plugin is installed on the build machine this resolves to the real
 * module; otherwise it rejects with a clear error message.
 */
export async function writeTextFile(path: string, contents: string): Promise<void> {
  const mod = await import('@tauri-apps/plugin-fs')
  return mod.writeTextFile(path, contents)
}

export async function readTextFile(path: string): Promise<string> {
  const mod = await import('@tauri-apps/plugin-fs')
  return mod.readTextFile(path)
}
