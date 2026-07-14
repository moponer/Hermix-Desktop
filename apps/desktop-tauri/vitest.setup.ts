/**
 * vitest setup — wires @testing-library/jest-dom so every test file
 * gets the extended DOM matchers (`toBeInTheDocument`,
 * `toHaveTextContent`, etc.) for free.
 *
 * Added 2026-07-14 to unblock React component tests for
 * `CompactionStatusBanner` (Phase 5 reconnect). Without this, tests
 * error with "Invalid Chai property: toBeInTheDocument" because
 * the matcher lives in a separate package.
 */
import "@testing-library/jest-dom/vitest";