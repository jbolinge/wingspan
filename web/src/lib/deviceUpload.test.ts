import { describe, expect, it } from 'vitest'
import {
  buildSelection,
  buildUploadForm,
  capMessage,
  entriesFromFileList,
  fmtBytes,
  layoutKind,
  MAX_FILE_BYTES,
  selectionSentence,
  stripPickedFolder,
  type DeviceEntry,
} from './deviceUpload'

/**
 * jsdom cannot run a real directory pick, so the files the browser would hand
 * over are built by hand: `webkitRelativePath` is read-only, hence the
 * `defineProperty`, and an outsized file gets a fake `size` rather than four
 * real megabytes.
 */
function fileAt(path: string, bytes = 100): File {
  const name = path.split('/').pop() as string
  const file = new File(['x'.repeat(Math.min(bytes, 4096))], name, { type: 'text/plain' })
  Object.defineProperty(file, 'webkitRelativePath', { value: path })
  Object.defineProperty(file, 'size', { value: bytes })
  return file
}

function entries(...paths: string[]): DeviceEntry[] {
  return entriesFromFileList(paths.map((path) => fileAt(path)))
}

describe('stripPickedFolder', () => {
  it('drops the picked folder segment and keeps the rest', () => {
    expect(stripPickedFolder('Trades/Open Trades/M1 July 14 2026')).toEqual({
      folder: 'Trades',
      relativePath: 'Open Trades/M1 July 14 2026',
    })
  })

  it('has nothing to strip for a bare filename', () => {
    expect(stripPickedFolder('notes.txt')).toBeNull()
    expect(stripPickedFolder('Trades/')).toBeNull()
  })
})

describe('layoutKind', () => {
  it('keeps files sitting directly in one of the three folders', () => {
    expect(layoutKind('Open Trades/M1')).toBe('trade')
    expect(layoutKind('Closed Trades/L2')).toBe('trade')
    expect(layoutKind('Daily Trade Statement/2026-08-28-AccountStatement.csv')).toBe('statement')
  })

  it('matches the folder names case-insensitively, like discovery', () => {
    expect(layoutKind('open trades/M1')).toBe('trade')
    expect(layoutKind('DAILY TRADE STATEMENT/x.csv')).toBe('statement')
  })

  it('allows exactly one level of descent', () => {
    expect(layoutKind('2026/Open Trades/M1')).toBe('trade')
    expect(layoutKind('archive/2026/Open Trades/M1')).toBeNull()
  })

  it('rejects anything that is not in one of the folders', () => {
    expect(layoutKind('Open Trades')).toBeNull()
    expect(layoutKind('Notes/M1')).toBeNull()
    expect(layoutKind('Open Trades/nested/M1')).toBeNull()
  })
})

describe('buildSelection', () => {
  it('counts trade files and statements, and names the picked folder', () => {
    const selection = buildSelection(
      entries(
        'Trades/Open Trades/M1 July 14 2026',
        'Trades/Open Trades/M2 Aug 3 2026',
        'Trades/Closed Trades/L1 May 2026',
        'Trades/Daily Trade Statement/2026-08-28-AccountStatement.csv',
      ),
    )
    expect(selection.folder).toBe('Trades')
    expect(selection.tradeFiles).toBe(3)
    expect(selection.statements).toBe(1)
    expect(selection.skipped).toHaveLength(0)
    expect(selection.bytes).toBe(400)
    expect(selection.kept.map((file) => file.relativePath)).toEqual([
      'Open Trades/M1 July 14 2026',
      'Open Trades/M2 Aug 3 2026',
      'Closed Trades/L1 May 2026',
      'Daily Trade Statement/2026-08-28-AccountStatement.csv',
    ])
  })

  it('skips dotfiles, files outside the layout, and files over the per-file cap', () => {
    const big = fileAt('Trades/Open Trades/huge', MAX_FILE_BYTES + 1)
    const selection = buildSelection([
      ...entries(
        'Trades/Open Trades/M1',
        'Trades/.DS_Store',
        'Trades/Open Trades/.hidden',
        'Trades/README.md',
        'Trades/archive/2025/Open Trades/M0',
      ),
      { path: 'Trades/Open Trades/huge', file: big },
    ])

    expect(selection.kept.map((file) => file.relativePath)).toEqual(['Open Trades/M1'])
    expect(selection.skipped).toEqual([
      { relativePath: '.DS_Store', reason: 'dotfile', bytes: 100 },
      { relativePath: 'Open Trades/.hidden', reason: 'dotfile', bytes: 100 },
      { relativePath: 'README.md', reason: 'outside-layout', bytes: 100 },
      { relativePath: 'archive/2025/Open Trades/M0', reason: 'outside-layout', bytes: 100 },
      { relativePath: 'Open Trades/huge', reason: 'too-large', bytes: MAX_FILE_BYTES + 1 },
    ])
    expect(selection.bytes).toBe(100)
    expect(selection.overCap).toBeNull()
  })

  it('flags a kept set that already busts a server cap', () => {
    const many = buildSelection(
      Array.from({ length: 401 }, (_, i) => ({
        path: `Trades/Open Trades/M${i}`,
        file: fileAt(`Trades/Open Trades/M${i}`, 10),
      })),
    )
    expect(many.overCap).toBe('files')
    expect(capMessage(many)).toContain('at most 400')

    // Nine files each at the per-file cap: every one is kept, the total is not.
    const heavy = buildSelection(
      Array.from({ length: 9 }, (_, i) => ({
        path: `Trades/Open Trades/M${i}`,
        file: fileAt(`Trades/Open Trades/M${i}`, MAX_FILE_BYTES),
      })),
    )
    expect(heavy.overCap).toBe('bytes')
    expect(capMessage(heavy)).toContain('32.0 MiB')
  })

  it('falls back to the raw path for a file with no folder at all', () => {
    const selection = buildSelection(entries('loose.csv'))
    expect(selection.folder).toBe('')
    expect(selection.skipped).toEqual([
      { relativePath: 'loose.csv', reason: 'outside-layout', bytes: 100 },
    ])
  })
})

describe('buildUploadForm', () => {
  it('sends one `file` part per kept file, named by its relative path', () => {
    const selection = buildSelection(
      entries('Trades/Open Trades/M1', 'Trades/README.md', 'Trades/Daily Trade Statement/s.csv'),
    )
    const form = buildUploadForm(selection)
    const parts = form.getAll('file').filter((part): part is File => part instanceof File)

    expect(form.getAll('file')).toHaveLength(2)
    expect(parts.map((part) => part.name)).toEqual([
      'Open Trades/M1',
      'Daily Trade Statement/s.csv',
    ])
  })
})

describe('formatting', () => {
  it('reports sizes in the units the caps are written in', () => {
    expect(fmtBytes(512)).toBe('512 B')
    expect(fmtBytes(2048)).toBe('2.0 KiB')
    expect(fmtBytes(4 * 1024 * 1024)).toBe('4.0 MiB')
    expect(fmtBytes(-1)).toBe('—')
  })

  it('summarises a selection in one line', () => {
    const selection = buildSelection(
      entries('Trades/Open Trades/M1', 'Trades/Daily Trade Statement/s.csv', 'Trades/README.md'),
    )
    expect(selectionSentence(selection)).toBe('1 trade file / 1 statement, 1 skipped, 200 B')
  })
})
