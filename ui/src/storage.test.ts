import { act, renderHook } from '@testing-library/react'
import { describe, expect, it, vi } from 'vitest'
import { z } from 'zod'
import { usePersisted } from './storage'

describe('usePersisted', () => {
  it('starts from the given value when nothing was kept', () => {
    const { result } = renderHook(() => usePersisted('draft', z.string(), 'ping'))
    expect(result.current[0]).toBe('ping')
  })

  it('hands the same value back to the next mount', () => {
    const first = renderHook(() => usePersisted('draft', z.string(), 'ping'))
    act(() => first.result.current[1]('what is the weather'))
    first.unmount()

    const second = renderHook(() => usePersisted('draft', z.string(), 'ping'))
    expect(second.result.current[0]).toBe('what is the weather')
  })

  it('namespaces what it writes, so it shares the origin without fighting over it', () => {
    const { result } = renderHook(() => usePersisted('maxTurns', z.number(), 6))
    act(() => result.current[1](12))
    expect(window.localStorage.getItem('mire.maxTurns')).toBe('12')
  })

  it('falls back rather than trusting a value of the wrong shape', () => {
    // What an older build left behind, or a hand-edited entry.
    window.localStorage.setItem('mire.maxTurns', '"six"')
    const { result } = renderHook(() => usePersisted('maxTurns', z.number(), 6))
    expect(result.current[0]).toBe(6)
  })

  it('survives storage that is not there, which is a reason to forget and not to fail', () => {
    const broken = vi.spyOn(window.localStorage, 'getItem').mockImplementation(() => {
      throw new Error('storage is disabled')
    })
    const full = vi.spyOn(window.localStorage, 'setItem').mockImplementation(() => {
      throw new Error('quota exceeded')
    })

    const { result } = renderHook(() => usePersisted('draft', z.string(), 'ping'))
    expect(result.current[0]).toBe('ping')
    expect(() => act(() => result.current[1]('still works'))).not.toThrow()
    expect(result.current[0]).toBe('still works')

    broken.mockRestore()
    full.mockRestore()
  })
})
