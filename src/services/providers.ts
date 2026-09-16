/**
 * Typed wrappers over the provider commands (spec sections 5, 12, 17).
 *
 * Command names and argument shapes mirror `src-tauri/src/commands/providers.rs`.
 * Note what is *not* here: there is no command that returns an API key. The key
 * is written once (`setSecret`) and can only be queried for existence
 * (`secretStatus`, a boolean).
 *
 * `create`/`update` forward the caller's `ProviderInput` object verbatim - no
 * field is copied into a new shape here. That matters for `maxContextTokens`:
 * the backend treats an absent field as `null`, so rebuilding the payload
 * without it would clear a declared context window instead of updating it.
 */

import { call } from "./backend";
import type {
  ProviderInput,
  ProviderProfile,
  ProviderTestResult,
} from "../types";

export const providersApi = {
  /** All configured provider profiles (metadata only). */
  list: (): Promise<ProviderProfile[]> => call<ProviderProfile[]>("list_providers"),

  /**
   * Create a profile. The API key is set separately via `setSecret`.
   *
   * `input` is sent as-is: it carries every profile field, `maxContextTokens`
   * included (or `null` when the user declared no window).
   */
  create: (input: ProviderInput): Promise<ProviderProfile> =>
    call<ProviderProfile>("create_provider", { input }),

  /**
   * Update a profile's metadata; the stored key is untouched.
   *
   * Also sent as-is, so the declared context window is updated rather than
   * silently dropped.
   */
  update: (id: string, input: ProviderInput): Promise<ProviderProfile> =>
    call<ProviderProfile>("update_provider", { id, input }),

  /** Delete a profile and its keyring entry. */
  remove: (id: string): Promise<void> => call<void>("delete_provider", { id }),

  /**
   * Store or clear a provider's credential in the OS keyring.
   *
   * The stored value is presented to the session as a *bearer* token
   * (`ANTHROPIC_AUTH_TOKEN`), not as an `x-api-key` header. An empty `apiKey`
   * clears the stored credential.
   */
  setSecret: (providerId: string, apiKey: string): Promise<void> =>
    call<void>("set_provider_secret", { providerId, apiKey }),

  /** Whether a key is stored for this provider. Never returns the key itself. */
  secretStatus: (providerId: string): Promise<boolean> =>
    call<boolean>("provider_secret_status", { providerId }),

  /** Authenticated connectivity check against the provider's base URL. */
  test: (providerId: string): Promise<ProviderTestResult> =>
    call<ProviderTestResult>("test_provider", { providerId }),
};
