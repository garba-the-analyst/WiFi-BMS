// Who is signed in and what they may do. The server decides every request; this only shapes the menu
// and keeps people from landing on pages that would answer 403.
import { reactive } from 'vue'
import { admin, adminToken } from './api'

export const session = reactive({ me: null })

export async function loadMe(force = false) {
  if (!adminToken.get()) { session.me = null; return null }
  if (session.me && !force) return session.me
  session.me = await admin.me()
  return session.me
}
export const clearSession = () => { session.me = null }
export const can = (perm) => !!session.me && session.me.permissions.includes(perm)
/** First page a person should see: reps have no dashboard, so they start at the customers list. */
export const homePath = () => (can('dashboard') ? '/admin' : '/admin/customers')
