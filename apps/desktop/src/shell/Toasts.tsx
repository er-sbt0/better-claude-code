import { useEffect } from 'react'

// The corner where the app says something went wrong, or that something
// happened while nobody was looking.
//
// §8 is the reason this is sticky rather than a three-second fade: a user who
// left three runs going is *entitled to be told* they are gone. A notice that
// disappears on its own is not telling anyone anything.

export type Toast = {
  id: number
  kind: 'error' | 'notice'
  title: string
  body?: string
}

let nextId = 0
export const toast = (kind: Toast['kind'], title: string, body?: string): Toast => ({
  id: nextId++,
  kind,
  title,
  body,
})

export function Toasts({
  toasts,
  onDismiss,
}: {
  toasts: Toast[]
  onDismiss: (id: number) => void
}) {
  // The newest is the one worth reading; the stack grows upwards from it.
  return (
    <div className="pointer-events-none absolute right-3 bottom-9 z-40 flex w-80 flex-col-reverse gap-2">
      {toasts.map((t) => (
        <Item key={t.id} toast={t} onDismiss={onDismiss} />
      ))}
    </div>
  )
}

function Item({ toast: t, onDismiss }: { toast: Toast; onDismiss: (id: number) => void }) {
  // A notice can age out; an error cannot. Whoever caused an error is owed the
  // text of it for as long as they need to read it.
  useEffect(() => {
    if (t.kind === 'error') return
    const timer = setTimeout(() => onDismiss(t.id), 12_000)
    return () => clearTimeout(timer)
  }, [t.id, t.kind, onDismiss])

  return (
    <div
      className={[
        'pointer-events-auto rounded-md border bg-elevated px-3 py-2 shadow-lg',
        t.kind === 'error' ? 'border-err/50' : 'border-border',
      ].join(' ')}
    >
      <div className="flex items-start gap-2">
        <span
          className={`mt-1 size-1.5 shrink-0 rounded-full ${t.kind === 'error' ? 'bg-err' : 'bg-warn'}`}
          aria-hidden="true"
        />
        <div className="min-w-0 flex-1">
          <p className={`text-micro font-semibold ${t.kind === 'error' ? 'text-err' : 'text-text'}`}>
            {t.title}
          </p>
          {t.body !== undefined && (
            <p className="mt-1 font-mono text-mono break-words text-muted select-text">{t.body}</p>
          )}
        </div>
        <button
          type="button"
          onClick={() => onDismiss(t.id)}
          className="-mt-0.5 shrink-0 rounded-sm px-1 text-muted hover:bg-hover hover:text-text"
          aria-label="Dismiss"
        >
          ×
        </button>
      </div>
    </div>
  )
}
