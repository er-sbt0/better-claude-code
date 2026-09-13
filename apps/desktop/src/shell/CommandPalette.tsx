import { useEffect, useMemo, useRef, useState } from 'react'
import { useEscape } from './Dock'

// ⌘K. The furniture that makes an application feel like one: every command the
// chrome offers, reachable by typing part of its name.
//
// Deliberately *not* a keymap. A full keyboard layer — focus cycling, a
// bindings file, chords — is a larger piece of work and was scoped out; this
// is one shortcut that opens a list, and every entry in the list is something
// a button elsewhere also does.

export type Command = {
  id: string
  title: string
  /** The group heading it files under, e.g. `Run`, `View`. */
  group: string
  /** Shown right-aligned. A hint, not a registered binding. */
  hint?: string
  disabled?: boolean
  run: () => void
}

export function CommandPalette({
  open,
  commands,
  onClose,
}: {
  open: boolean
  commands: Command[]
  onClose: () => void
}) {
  const [query, setQuery] = useState('')
  const [cursor, setCursor] = useState(0)
  const input = useRef<HTMLInputElement | null>(null)

  useEscape(open, onClose)

  useEffect(() => {
    if (!open) return
    setQuery('')
    setCursor(0)
    // The palette is useless until it has focus, and a webview does not give
    // it to a node that was mounted in the same frame.
    const timer = setTimeout(() => input.current?.focus(), 0)
    return () => clearTimeout(timer)
  }, [open])

  const matches = useMemo(() => {
    const q = query.trim().toLowerCase()
    const hits =
      q === ''
        ? commands
        : commands.filter((c) => `${c.group} ${c.title}`.toLowerCase().includes(q))
    return hits.filter((c) => c.disabled !== true).concat(hits.filter((c) => c.disabled === true))
  }, [commands, query])

  if (!open) return null

  const choose = (command: Command | undefined) => {
    if (command === undefined || command.disabled === true) return
    onClose()
    command.run()
  }

  const onKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      setCursor((c) => Math.min(c + 1, matches.length - 1))
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      setCursor((c) => Math.max(c - 1, 0))
    } else if (e.key === 'Enter') {
      e.preventDefault()
      choose(matches[cursor])
    }
  }

  return (
    <div
      className="absolute inset-0 z-50 flex items-start justify-center bg-black/40 pt-[12vh]"
      onMouseDown={onClose}
    >
      <div
        className="w-[34rem] max-w-[calc(100%-2rem)] overflow-hidden rounded-md border border-border bg-elevated shadow-2xl"
        onMouseDown={(e) => e.stopPropagation()}
      >
        <input
          ref={input}
          value={query}
          onChange={(e) => {
            setQuery(e.target.value)
            setCursor(0)
          }}
          onKeyDown={onKeyDown}
          placeholder="Run a command…"
          className="w-full border-b border-border bg-transparent px-3 py-2.5 text-text outline-none placeholder:text-faint"
        />
        <ol className="max-h-80 overflow-y-auto py-1">
          {matches.length === 0 && (
            <li className="px-3 py-2 text-micro text-faint">No matching command</li>
          )}
          {matches.map((c, i) => (
            <li key={c.id}>
              <button
                type="button"
                disabled={c.disabled}
                onMouseMove={() => setCursor(i)}
                onClick={() => choose(c)}
                className={[
                  'flex w-full items-center gap-2 px-3 py-1.5 text-left',
                  c.disabled === true ? 'text-faint' : 'text-text',
                  i === cursor && c.disabled !== true ? 'bg-selected' : '',
                ].join(' ')}
              >
                <span className="w-20 shrink-0 text-micro text-muted">{c.group}</span>
                <span className="min-w-0 flex-1 truncate">{c.title}</span>
                {c.hint !== undefined && (
                  <kbd className="shrink-0 rounded-sm border border-border px-1 font-mono text-[10px] text-faint">
                    {c.hint}
                  </kbd>
                )}
              </button>
            </li>
          ))}
        </ol>
      </div>
    </div>
  )
}

/** ⌘K / Ctrl-K, wherever focus happens to be. */
export function usePaletteShortcut(toggle: () => void) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === 'k') {
        e.preventDefault()
        toggle()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [toggle])
}
