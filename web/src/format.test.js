import { describe, it, expect } from 'vitest'
import { naira, bytes, duration, plausiblePhone, safeHotspotLink } from './format'

describe('format', () => {
  it('shows naira from kobo', () => {
    expect(naira(150000)).toBe('₦1,500')
    expect(naira(12050)).toBe('₦120.50')
    expect(naira(0)).toBe('₦0')
  })
  it('shows sizes', () => {
    expect(bytes(0)).toBe('0 B')
    expect(bytes(1536)).toBe('1.5 KB')
    expect(bytes(5 * 1024 ** 3)).toBe('5.0 GB')
  })
  it('shows durations', () => {
    expect(duration(3600)).toBe('1 h')
    expect(duration(5400)).toBe('1 h 30 min')
    expect(duration(172800)).toBe('2 days')
    expect(duration(45)).toBe('45 s')
  })
  it('checks phones loosely', () => {
    expect(plausiblePhone('0801 234 5678')).toBe(true)
    expect(plausiblePhone('+2348012345678')).toBe(true)
    expect(plausiblePhone('12345')).toBe(false)
  })
})

describe('safeHotspotLink', () => {
  it('accepts local router addresses', () => {
    expect(safeHotspotLink('http://192.168.88.1/login')).toBeTruthy()
    expect(safeHotspotLink('http://10.5.50.1/login')).toBeTruthy()
    expect(safeHotspotLink('http://172.20.0.1/login')).toBeTruthy()
    expect(safeHotspotLink('http://hotspot.lan/login')).toBeTruthy()
  })
  it('refuses anything that could leak the ticket', () => {
    expect(safeHotspotLink('https://evil.example/login')).toBeNull()
    expect(safeHotspotLink('http://8.8.8.8/login')).toBeNull()
    expect(safeHotspotLink('http://172.32.0.1/login')).toBeNull()
    expect(safeHotspotLink('javascript:alert(1)')).toBeNull()
    expect(safeHotspotLink('http://192.168.1.1@evil.example/')).toBeNull()
    expect(safeHotspotLink('')).toBeNull()
  })
  it('allows hosts the operator lists', () => {
    expect(safeHotspotLink('http://login.wifi.example/login', ['login.wifi.example'])).toBeTruthy()
  })
})
