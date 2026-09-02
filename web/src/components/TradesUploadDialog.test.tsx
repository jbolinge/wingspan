/**
 * The one way trades reach the app: pick a folder on this device, see exactly
 * what will leave it, upload. There is no second tab and no route that lists
 * the server's own folders — that feature was removed.
 */

import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import { ApiError, apiUpload } from '../api/client'
import { mockRequest, resetMockTradesRoot } from '../api/mock'
import { renderWithClient, resetAppStore } from '../test/utils'
import { TradesUploadDialog } from './TradesUploadDialog'

// The device panel's 413/422 branches are exactly the cases the client-side
// filter is meant to prevent, so they cannot be provoked through the mock
// router: the upload call itself is the seam.
vi.mock('../api/client', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../api/client')>()
  return { ...actual, apiUpload: vi.fn(actual.apiUpload) }
})

/** A file as a `webkitdirectory` pick would hand it over. */
function fileAt(path: string, bytes = 120): File {
  const name = path.split('/').pop() as string
  const file = new File(['x'.repeat(bytes)], name, { type: 'text/plain' })
  Object.defineProperty(file, 'webkitRelativePath', { value: path })
  return file
}

const PICK = [
  fileAt('Trades/Open Trades/Oct Strangle Aug 24 2026'),
  fileAt('Trades/Open Trades/Nov Condor Aug 26 2026'),
  fileAt('Trades/Daily Trade Statement/2026-08-28-AccountStatement.csv'),
  fileAt('Trades/README.md'),
  fileAt('Trades/.DS_Store'),
]

async function openDialog(): Promise<ReturnType<typeof userEvent.setup>> {
  const user = userEvent.setup()
  renderWithClient(<TradesUploadDialog onClose={vi.fn()} />)
  await screen.findByRole('button', { name: 'Choose folder…' })
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

describe('the upload dialog', () => {
  it('is the only way in: one panel, no server-side folder browser', async () => {
    renderWithClient(<TradesUploadDialog onClose={vi.fn()} />)

    expect(await screen.findByRole('button', { name: 'Choose folder…' })).toBeInTheDocument()
    // The tabs and the server-side picker they led to are gone.
    expect(screen.queryByRole('tablist')).not.toBeInTheDocument()
    expect(screen.queryByRole('tab')).not.toBeInTheDocument()
    expect(screen.queryByLabelText('Folder path')).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '⌂ Home' })).not.toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Select' })).not.toBeInTheDocument()
  })

  it('names the dialog for what it does', async () => {
    renderWithClient(<TradesUploadDialog onClose={vi.fn()} />)
    const dialog = await screen.findByRole('dialog', { name: 'Load trades from this device' })
    expect(dialog).toHaveTextContent(/Open Trades \/ Closed Trades \/ Daily Trade Statement/)
  })

  it('says out loud that an upload is only a snapshot', async () => {
    await openDialog()
    expect(screen.getByText(/snapshot — edits you make on this device/i)).toBeInTheDocument()
  })
})

describe('TradesDeviceTab', () => {
  it('summarises the pick before anything is uploaded', async () => {
    const user = await openDialog()
    await pickFolder(user)

    const summary = screen.getByRole('group', { name: 'Upload summary' })
    expect(within(summary).getByText('Trades')).toBeInTheDocument()
    expect(within(summary).getByText(/2 trade files \/ 1 statement/)).toBeInTheDocument()
    expect(
      within(summary).getByText(/2 \(1 outside the trades layout, 1 hidden\)/),
    ).toBeInTheDocument()
    expect(within(summary).getByText('360 B')).toBeInTheDocument()
    // The kept files are named, so it is obvious what leaves the device.
    expect(screen.getByText('Open Trades/Oct Strangle Aug 24 2026')).toBeInTheDocument()
    expect(screen.queryByText('README.md')).not.toBeInTheDocument()
  })

  it('uploads the snapshot and confirms what came back', async () => {
    const user = await openDialog()
    await pickFolder(user)
    await user.click(screen.getByRole('button', { name: 'Upload & analyze' }))

    expect(await screen.findByRole('status')).toHaveTextContent(
      /\d+ open \/ \d+ closed trades loaded from this device/,
    )
    // The server is now serving the uploaded copy.
    expect(mockRequest('/trades/root')).toMatchObject({ source: 'uploaded' })
  })

  it('sends one part per kept file, named by its path inside the picked folder', async () => {
    const user = await openDialog()
    await pickFolder(user)
    await user.click(screen.getByRole('button', { name: 'Upload & analyze' }))
    await screen.findByRole('status')

    const [path, form] = vi.mocked(apiUpload).mock.calls[0] as [string, FormData]
    expect(path).toBe('/trades/upload')
    const parts = form.getAll('file').filter((part): part is File => part instanceof File)
    expect(parts.map((part) => part.name)).toEqual([
      'Open Trades/Oct Strangle Aug 24 2026',
      'Open Trades/Nov Condor Aug 26 2026',
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
    const user = await openDialog()
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
    const user = await openDialog()
    await pickFolder(user)
    await user.click(screen.getByRole('button', { name: 'Upload & analyze' }))

    const alert = await screen.findByRole('alert')
    expect(alert).toHaveTextContent('No trades layout in that folder.')
    expect(alert).toHaveTextContent('looked for "Open Trades"')
    // Still on the device tab, with the pick intact, ready to retry.
    expect(screen.getByRole('group', { name: 'Upload summary' })).toBeInTheDocument()
  })

  it('refuses to send a pick that has nothing in the trades layout', async () => {
    const user = await openDialog()
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
    const user = await openDialog()

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
    await openDialog()
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
    form.append('file', new File(['abc'], 'strangle'), 'Open Trades/Oct Strangle')
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
