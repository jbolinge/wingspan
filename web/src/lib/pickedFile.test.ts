import { beforeEach, describe, expect, it } from 'vitest'
import { resetPickedFile, usePickedFile } from './pickedFile'

beforeEach(() => {
  resetPickedFile()
})

describe('usePickedFile', () => {
  it('starts with nothing pending', () => {
    expect(usePickedFile.getState().pending).toBeNull()
  })

  it('receive parks the outcome; claim returns it once and then nothing', () => {
    usePickedFile.getState().receive({ kind: 'file', path: '/home/trader/Downloads/legs.csv' })

    expect(usePickedFile.getState().pending?.outcome).toEqual({
      kind: 'file',
      path: '/home/trader/Downloads/legs.csv',
    })

    expect(usePickedFile.getState().claim()).toEqual({
      kind: 'file',
      path: '/home/trader/Downloads/legs.csv',
    })
    expect(usePickedFile.getState().pending).toBeNull()
    expect(usePickedFile.getState().claim()).toBeNull()
  })

  it('parks an error outcome the same way', () => {
    usePickedFile.getState().receive({ kind: 'error', detail: 'that file is not a CSV' })

    expect(usePickedFile.getState().claim()).toEqual({
      kind: 'error',
      detail: 'that file is not a CSV',
    })
    expect(usePickedFile.getState().claim()).toBeNull()
  })

  it('two identical picks in a row are two distinct changes', () => {
    const seen: unknown[] = []
    const unsubscribe = usePickedFile.subscribe((state) => seen.push(state.pending))

    usePickedFile.getState().receive({ kind: 'file', path: '/same.csv' })
    usePickedFile.getState().receive({ kind: 'file', path: '/same.csv' })

    unsubscribe()
    // Each receive sets a fresh object, so a subscriber sees two updates
    // even though the outcome inside them is equal.
    expect(seen.length).toBe(2)
    expect(seen[0]).not.toBe(seen[1])
  })

  it('resetPickedFile forgets an outcome nobody claimed', () => {
    usePickedFile.getState().receive({ kind: 'file', path: '/x.csv' })
    resetPickedFile()
    expect(usePickedFile.getState().pending).toBeNull()
  })
})
