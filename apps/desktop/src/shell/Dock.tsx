import { useCallback, useEffect, useRef, type ReactNode } from 'react'

// A dock: a pane pinned to one side of the window, collapsible and resizable
// by dragging its inner edge. Zed's model, and the reason the app is no longer
// a scrolling page — the window is a frame, and every scrolling region is
// inside one of these.

const MIN = 180
const MAX = 520

export function Dock({
  side,
  width,
  onResize,
  open,
  children,
}: {
  side: 'left' | 'right'
  width: number
  onResize: (width: number) => void
  open: boolean
  children: ReactNode
}) {
  const ref = useRef<HTMLElement | null>(null)

  // Pointer capture rather than window listeners: a drag that leaves the
  // window still belongs to this handle, and releasing outside still ends it.
  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      e.preventDefault()
      const handle = e.currentTarget
      handle.setPointerCapture(e.pointerId)
      const startX = e.clientX
      const startWidth = width

      const move = (ev: PointerEvent) => {
        const delta = side === 'left' ? ev.clientX - startX : startX - ev.clientX
        onResize(Math.max(MIN, Math.min(MAX, startWidth + delta)))
      }
      const up = () => {
        handle.releasePointerCapture(e.pointerId)
        handle.removeEventListener('pointermove', move)
        handle.removeEventListener('pointerup', up)
      }
      handle.addEventListener('pointermove', move)
      handle.addEventListener('pointerup', up)
    },
    [onResize, side, width],
  )

  // A collapsed dock is removed from the layout, not hidden underneath it:
  // `display: none` would leave its scroll positions and focus ring alive.
  if (!open) return null

  return (
    <aside
      ref={ref}
      style={{ width }}
      className={[
        'relative flex h-full shrink-0 flex-col overflow-hidden bg-panel',
        side === 'left' ? 'border-r border-border' : 'border-l border-border',
      ].join(' ')}
    >
      <div className="flex min-h-0 flex-1 flex-col overflow-hidden">{children}</div>
      <div
        role="separator"
        aria-orientation="vertical"
        onPointerDown={onPointerDown}
        onDoubleClick={() => onResize(side === 'left' ? 240 : 300)}
        className={[
          'absolute top-0 z-10 h-full w-1 cursor-col-resize hover:bg-accent/60',
          side === 'left' ? 'right-0' : 'left-0',
        ].join(' ')}
      />
    </aside>
  )
}

/** A dock's section heading. Small, spaced, upper-case — Zed's, and Orca's. */
export function DockSection({
  title,
  actions,
  children,
}: {
  title: string
  actions?: ReactNode
  children: ReactNode
}) {
  return (
    <section className="flex min-h-0 flex-col">
      <header className="flex h-7 shrink-0 items-center justify-between gap-2 px-3 text-micro font-semibold tracking-label text-muted uppercase">
        <span className="truncate">{title}</span>
        {actions}
      </header>
      <div className="min-h-0 overflow-y-auto">{children}</div>
    </section>
  )
}

/** The empty state of a dock section: one grey sentence, never a blank box. */
export function DockEmpty({ children }: { children: ReactNode }) {
  return <p className="px-3 pb-2 text-micro text-faint">{children}</p>
}

/** Escape closes the topmost overlay. Used by the palette and the dialog. */
export function useEscape(active: boolean, close: () => void) {
  useEffect(() => {
    if (!active) return
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        close()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [active, close])
}
