import { describe, it, expect } from 'vitest'
import { naira, bytes, duration, plausiblePhone, safeHotspotLink, mixHex, isHexColour, planSummary, ago, roleLabel, signalQuality, kindLabel } from './format'

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

describe('brand colour', () => {
  it('validates and mixes hex colours', () => {
    expect(isHexColour('#0F766E')).toBe(true)
    expect(isHexColour('red')).toBe(false)
    expect(mixHex('#000000', '#ffffff', 0.5)).toBe('#808080')
    expect(mixHex('#0F766E', '#0F766E', 0.7)).toBe('#0f766e')
  })
})

describe('plan and staff wording', () => {
  it('summarises time, data, devices and speed', () => {
    expect(planSummary({ active_seconds: 86400, data_bytes: null, device_limit: 1, down_kbps: 2000, up_kbps: 2000 })).toBe('1 day of use · 1 device · 2 Mbps down / 2 Mbps up')
    expect(planSummary({ active_seconds: null, data_bytes: 5 * 1024 ** 3, device_limit: 3, down_kbps: 5000, up_kbps: 1000 })).toBe('5.0 GB · 3 devices · 5 Mbps down / 1 Mbps up')
  })
  it('says how long ago', () => {
    const now = Date.parse('2026-11-18T12:00:00Z')
    expect(ago('2026-11-18T11:59:30Z', now)).toBe('just now')
    expect(ago('2026-11-18T11:30:00Z', now)).toBe('30 min ago')
    expect(ago('2026-11-16T12:00:00Z', now)).toBe('2 d ago')
  })
  it('names roles', () => {
    expect(roleLabel('customer_rep')).toBe('Customer rep')
    expect(roleLabel('weird')).toBe('weird')
  })
})

describe('device wording', () => {
  it('grades radio signal with the same limits the server uses', () => {
    expect(signalQuality(-55)).toEqual({ label: 'strong', cls: 'ok' })
    expect(signalQuality(-70).label).toBe('fair')
    expect(signalQuality(-75).label).toBe('weak')
    expect(signalQuality(null).label).toBe('–')
  })
  it('names device kinds', () => {
    expect(kindLabel('cpe')).toBe('Radio link')
    expect(kindLabel('ap')).toBe('Access point')
    expect(kindLabel('x')).toBe('x')
  })
})
