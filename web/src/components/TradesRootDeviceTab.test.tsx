import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError, apiUpload } from '../api/client'
import { mockRequest, resetMockTradesRoot } from '../api/mock'
import type { TradesRootResponse } from '../api/types'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradesRootDialog } from './TradesRootDialog'

// The device tab's 413/422 branches are exactly the cases the client-side
// filter is meant to prevent, so they cannot be provoked through the mock
// router: the upload call itself is the seam.
vi.mock('../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api/client')>()
  return { ...actual, apiUpload: vi.fn(actual.apiUpload) }
})

const CONFIG_ROOT: TradesRootResponse = {
  root: '/home/trader/workspace/example_trades/Trades',
  source: 'config',
  config_root: '/home/trader/workspace/example_trades/Trades',
  open_trades: 6,
  closed_trades: 2,
}

/** A file as a `webkitdirectory` pick would hand it over. */
function fileAt(path: string, bytes = 120): File {
  const name = path.split('/').pop() as string
  const file = new File(['x'.repeat(bytes)], name, { type: 'text/plain' })
  Object.defineProperty(file, 'webkitRelativePath', { value: path })
  return file
}

const PICK = [
  fileAt('Trades/Open Trades/M1 July 14 2026'),
  fileAt('Trades/Open Trades/M2 Aug 3 2026'),
  fileAt('Trades/Daily Trade Statement/2026-08-28-AccountStatement.csv'),
  fileAt('Trades/README.md'),
  fileAt('Trades/.DS_Store'),
]

async function openDeviceTab(): Promise<ReturnType<typeof userEvent.setup>> {
  const user = userEvent.setup()
  renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} initialTab="device" />)
  await screen.findByRole('tab', { name: 'On this device', selected: true })
  return user
}

async function pickFolder(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.upload(screen.getByLabelText('Trades folder on this device'), PICK)
  await screen.findByRole('group', { name: 'Upload summary' })
}

beforeEach(() => {
  resetAppStore()
  resetMockTradesRoot()
})

describe('trades folder tabs', () => {
  it('offers both places the trade files could be, as a real tablist', async () => {
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const tablist = await screen.findByRole('tablist', { name: 'Where the trade files are' })
    const [onServer, onDevice] = within(tablist).getAllByRole('tab')
    expect(onServer).toHaveAttribute('aria-selected', 'true')
    expect(onDevice).toHaveAttribute('aria-selected', 'false')
    // Roving tabindex: the strip is one tab stop.
    expect(onServer).toHaveAttribute('tabindex', '0')
    expect(onDevice).toHaveAttribute('tabindex', '-1')
    const panel = screen.getByRole('tabpanel')
    expect(panel).toHaveAttribute('aria-labelledby', (onServer as HTMLElement).id)
  })

  it('moves between tabs with the arrow keys, and Home/End', async () => {
    const user = userEvent.setup()
    renderWithClient(<TradesRootDialog root={CONFIG_ROOT} onClose={vi.fn()} />)

    const onServer = await screen.findByRole('tab', { name: 'On server' })
    onServer.focus()
    await user.keyboard('{ArrowRight}')

    const onDevice = screen.getByRole('tab', { name: 'On this device' })
    expect(onDevice).toHaveAttribute('aria-selected', 'true')
    expect(document.activeElement).toBe(onDevice)
    expect(screen.getByRole('button', { name: 'Choose folder…' })).toBeInTheDocument()
    expect(screen.queryByLabelText('Folder path')).not.toBeInTheDocument()

    await user.keyboard('{Home}')
    expect(screen.getByRole('tab', { name: 'On server' })).toHaveAttribute('aria-selected', 'true')
    expect(screen.getByLabelText('Folder path')).toBeInTheDocument()

    await user.keyboard('{End}')
    expect(screen.getByRole('tab', { name: 'On this device' })).toHaveAttribute(
      'aria-selected',
      'true',
    )
  })

  it('says out loud that an upload is only a snapshot', async () => {
    await openDeviceTab()
    expect(screen.getByText(/snapshot — edits you make on this device/i)).toBeInTheDocument()
  })
})

describe('TradesDeviceTab', () => {
  it('summarises the pick before anything is uploaded', async () => {
    const user = await openDeviceTab()
    await pickFolder(user)

    const summary = screen.getByRole('group', { name: 'Upload summary' })
    expect(within(summary).getByText('Trades')).toBeInTheDocument()
    expect(within(summary).getByText(/2 trade files \/ 1 statement/)).toBeInTheDocument()
    expect(
      within(summary).getByText(/2 \(1 outside the trades layout, 1 hidden\)/),
    ).toBeInTheDocument()
    expect(within(summary).getByText('360 B')).toBeInTheDocument()
    // The kept files are named, so it is obvious what leaves the device.
    expect(screen.getByText('Open Trades/M1 July 14 2026')).toBeInTheDocument()
    expect(screen.queryByText('README.md')).not.toBeInTheDocument()
  })

  it('uploads the snapshot and confirms what came back', async () => {
    const user = await openDeviceTab()
    await pickFolder(user)
    await user.click(screen.getByRole('button', { name: 'Upload & analyze' }))

    expect(await screen.findByRole('status')).toHaveTextContent(
      /\d+ open \/ \d+ closed trades loaded from this device/,
    )
    // The server is now serving the uploaded copy.
    expect(mockRequest('/trades/root')).toMatchObject({ source: 'uploaded' })
  })

  it('sends one part per kept file, named by its path inside the picked folder', async () => {
    const user = await openDeviceTab()
    await pickFolder(user)
    await user.click(screen.getByRole('button', { name: 'Upload & analyze' }))
    await screen.findByRole('status')

    const [path, form] = vi.mocked(apiUpload).mock.calls[0] as [string, FormData]
    expect(path).toBe('/trades/upload')
    const parts = form.getAll('file').filter((part): part is File => part instanceof File)
    expect(parts.map((part) => part.name)).toEqual([
      'Open Trades/M1 July 14 2026',
      'Open Trades/M2 Aug 3 2026',
      'Daily Trade Statement/2026-08-28-AccountStatement.csv',
    ])
  })

  it('renders a 413 problem+json inline, naming the cap', async () => {
    vi.mocked(apiUpload).mockRejectedValueOnce(
      new ApiError('too large', 413, {
        title: 'Content Too Large',
        status: 413,
        detail: 'the upload is larger than the 32 MiB cap',
      }),
    )
    const user = await openDeviceTab()
    await pickFolder(user)
    await user.click(screen.getByRole('button', { name: 'Upload & analyze' }))

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('Too large for one upload.')
    expect(alert).toHaveTextContent('the upload is larger than the 32 MiB cap')
    expect(screen.queryByRole('status')).not.toBeInTheDocument()
  })

  it('renders a 422 problem+json inline, with the discovery sentence', async () => {
    vi.mocked(apiUpload).mockRejectedValueOnce(
      new ApiError('no layout', 422, {
        title: 'Unprocessable Entity',
        status: 422,
        detail:
          'no trades folders in the uploaded files (looked for "Open Trades", "Closed Trades" or "Daily Trade Statement", and one level down)',
      }),
    )
    const user = await openDeviceTab()
    await pickFolder(user)
    await user.click(screen.getByRole('button', { name: 'Upload & analyze' }))

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('No trades layout in that folder.')
    expect(alert).toHaveTextContent('looked for "Open Trades"')
    // Still on the device tab, with the pick intact, ready to retry.
    expect(screen.getByRole('group', { name: 'Upload summary' })).toBeInTheDocument()
  })

  it('refuses to send a pick that has nothing in the trades layout', async () => {
    const user = await openDeviceTab()
    await user.upload(screen.getByLabelText('Trades folder on this device'), [
      fileAt('Trades/README.md'),
      fileAt('Trades/notes/scratch.txt'),
    ])

    await waitFor(() =>
      expect(screen.getByText(/nothing in that folder belongs to the trades layout/i)).toBeInTheDocument(),
    )
    expect(screen.getByRole('button', { name: 'Upload & analyze' })).toBeDisabled()
  })
})

/**
 * Firefox and Safari have no `window.showDirectoryPicker`, which is also what
 * jsdom looks like — so this is the *only* path these tests ever exercise, and
 * "Choose folder…" must reach the hidden `webkitdirectory` input rather than
 * sit there doing nothing. The one-click re-sync pill belongs to the browsers
 * that can remember a folder, so it must not be promised here.
 */
describe('a browser without the File System Access API', () => {
  it('wires Choose folder… to the folder input instead of a dead button', async () => {
    expect('showDirectoryPicker' in window).toBe(false)
    const user = await openDeviceTab()

    const input = screen.getByLabelText('Trades folder on this device')
    const opened = vi.fn((event: Event) => event.preventDefault())
    input.addEventListener('click', opened)

    await user.click(screen.getByRole('button', { name: 'Choose folder…' }))
    expect(opened).toHaveBeenCalledTimes(1)
    // And nothing pretended to be reading a folder that never opened.
    expect(screen.queryByText(/reading the folder/i)).not.toBeInTheDocument()
    expect(screen.queryByRole('alert')).not.toBeInTheDocument()
  })

  it('does not offer the re-syncable badge it cannot honour', async () => {
    await openDeviceTab()
    expect(screen.queryByText('re-syncable')).not.toBeInTheDocument()
  })
})

describe('mock POST /trades/upload', () => {
  it('answers 422 when nothing in the upload is part of the trades layout', () => {
    const form = new FormData()
    form.append('file', new File(['x'], 'README.md'), 'README.md')
    expect(() => mockRequest('/trades/upload', { method: 'POST', body: form })).toThrow(
      /no trades folders in the uploaded files/,
    )
  })

  it('keeps the layout files, counts the rest as skipped and switches source', () => {
    const form = new FormData()
    form.append('file', new File(['abc'], 'M1'), 'Open Trades/M1')
    form.append('file', new File(['de'], 's.csv'), 'Daily Trade Statement/s.csv')
    form.append('file', new File(['f'], 'README.md'), 'README.md')
    form.append('file', new File(['g'], 'escape'), '../escape')

    expect(mockRequest('/trades/upload', { method: 'POST', body: form })).toMatchObject({
      source: 'uploaded',
      uploaded: { files: 2, bytes: 5, skipped: 2 },
    })
    expect(mockRequest('/trades/root')).toMatchObject({ source: 'uploaded' })
    expect(mockRequest('/trades/root', { method: 'DELETE' })).toMatchObject({ source: 'config' })
  })
})
