import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { apiUpload } from '../api/client'
import { mockRequest, resetMockTradesRoot } from '../api/mock'
import { rememberFolderForTest } from '../lib/deviceFolder'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradesRootControl } from './TradesRootControl'

vi.mock('../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api/client')>()
  return { ...actual, apiUpload: vi.fn(actual.apiUpload) }
})

/** Put the mock server into the state a device upload leaves it in. */
function uploadFromDevice(): void {
  const form = new FormData()
  form.append('file', new File(['abc'], 'M1'), 'Open Trades/M1 July 14 2026')
  form.append('file', new File(['de'], 's.csv'), 'Daily Trade Statement/2026-08-28.csv')
  mockRequest('/trades/upload', { method: 'POST', body: form })
}

/**
 * A stand-in for a File System Access directory handle: enough of one for the
 * walk that re-sync does. jsdom has no real one, and no way to obtain one.
 */
type Node = { [name: string]: Node | File }

function dirHandle(name: string, node: Node): unknown {
  return {
    kind: 'directory',
    name,
    async *values() {
      for (const [key, value] of Object.entries(node)) {
        yield value instanceof File
          ? { kind: 'file', name: key, getFile: () => Promise.resolve(value) }
          : (dirHandle(key, value) as { kind: 'directory' })
      }
    },
  }
}

async function tooltipOf(button: HTMLElement): Promise<HTMLElement> {
  await userEvent.setup().hover(button)
  return screen.findByRole('tooltip')
}

beforeEach(() => {
  resetAppStore()
  resetMockTradesRoot()
  rememberFolderForTest(null)
})

afterEach(() => {
  vi.mocked(apiUpload).mockClear()
})

describe('TradesRootControl with an uploaded root', () => {
  it('names the device, not the server-side staging path', async () => {
    uploadFromDevice()
    renderWithClient(<TradesRootControl />)

    const button = await screen.findByRole('button', { name: 'Trades folder: this device' })
    const tip = await tooltipOf(button)
    expect(tip).toHaveTextContent('Uploaded from this device')
    expect(tip).toHaveTextContent(/uploaded 0s ago/)
    expect(tip).toHaveTextContent(/\d+ open \/ \d+ closed/)
    expect(tip).toHaveTextContent(/snapshot: edits on this device arrive on the next re-sync/i)
    expect(button).toHaveAttribute('title', expect.stringContaining('uploaded from this device'))
  })

  it('offers Re-sync only while the root came from an upload', async () => {
    renderWithClient(<TradesRootControl />)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })
    expect(screen.queryByRole('button', { name: /re-sync/ })).not.toBeInTheDocument()

    resetMockTradesRoot()
    uploadFromDevice()
    const { unmount } = renderWithClient(<TradesRootControl />)
    expect(await screen.findByRole('button', { name: '↻ re-sync' })).toBeInTheDocument()
    unmount()
  })

  it('re-reads and re-uploads in one click when the folder is remembered', async () => {
    uploadFromDevice()
    rememberFolderForTest(
      dirHandle('Trades', {
        'Open Trades': { 'M1 July 14 2026': new File(['abc'], 'M1 July 14 2026') },
        'Daily Trade Statement': { 'x.csv': new File(['de'], 'x.csv') },
        '.git': { HEAD: new File(['ref'], 'HEAD') },
        'README.md': new File(['hi'], 'README.md'),
      }),
    )
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    await user.click(await screen.findByRole('button', { name: '↻ re-sync' }))

    await waitFor(() => expect(vi.mocked(apiUpload)).toHaveBeenCalledTimes(1))
    const [path, form] = vi.mocked(apiUpload).mock.calls[0] as [string, FormData]
    expect(path).toBe('/trades/upload')
    const parts = form.getAll('file').filter((part): part is File => part instanceof File)
    // The walk skips dot-directories and the filter drops what is not layout.
    expect(parts.map((part) => part.name)).toEqual([
      'Open Trades/M1 July 14 2026',
      'Daily Trade Statement/x.csv',
    ])
    // One click, no dialog.
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('falls back to re-picking on the device tab when nothing is remembered', async () => {
    uploadFromDevice()
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    // No File System Access API in jsdom, and no handle from a previous pick:
    // this is the Firefox/Safari path, and the path after any page reload.
    expect('showDirectoryPicker' in window).toBe(false)
    await user.click(await screen.findByRole('button', { name: '↻ re-sync' }))

    const dialog = await screen.findByRole('dialog')
    expect(
      await screen.findByRole('tab', { name: 'On this device', selected: true }),
    ).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Choose folder…' })).toBeInTheDocument()
    expect(vi.mocked(apiUpload)).not.toHaveBeenCalled()
    expect(dialog).toBeInTheDocument()
  })

  /**
   * The handle outlives the permission: Chromium drops read access to a
   * remembered directory on its own (a new tab, a revoked site setting), and
   * a re-sync that silently uploaded nothing — or blew up with a
   * `NotAllowedError` — would leave the user staring at a stale book. It has
   * to land back on the device tab, ready for a fresh pick.
   */
  it('falls back to re-picking when the remembered folder lost its permission', async () => {
    uploadFromDevice()
    const denied = {
      ...(dirHandle('Trades', {
        'Open Trades': { 'M1 July 14 2026': new File(['abc'], 'M1 July 14 2026') },
      }) as object),
      queryPermission: () => Promise.resolve('prompt' as PermissionState),
      requestPermission: () => Promise.resolve('denied' as PermissionState),
    }
    rememberFolderForTest(denied)
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    await user.click(await screen.findByRole('button', { name: '↻ re-sync' }))

    expect(
      await screen.findByRole('tab', { name: 'On this device', selected: true }),
    ).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Choose folder…' })).toBeInTheDocument()
    // Nothing was sent, and the folder we could not read is not an error to
    // shout about — the dialog *is* the answer.
    expect(vi.mocked(apiUpload)).not.toHaveBeenCalled()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('drops the snapshot with the dialog’s Reset action', async () => {
    uploadFromDevice()
    const user = userEvent.setup()
    renderWithClient(<TradesRootControl />)

    await user.click(await screen.findByRole('button', { name: 'Trades folder: this device' }))
    await user.click(await screen.findByRole('button', { name: 'Reset to config' }))

    expect(await screen.findByRole('status')).toHaveTextContent(/trades loaded from/)
    await screen.findByRole('button', { name: 'Trades folder: Trades' })
    expect(screen.queryByRole('button', { name: /re-sync/ })).not.toBeInTheDocument()
  })
})
