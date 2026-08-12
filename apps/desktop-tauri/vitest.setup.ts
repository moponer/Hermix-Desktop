/**
 * vitest setup — wires @testing-library/jest-dom so every test file
 * gets the extended DOM matchers (`toBeInTheDocument`,
 * `toHaveTextContent`, etc.) for free.
 *
 * React component tests for `CompactionStatusBanner` unblocked by this.
 * Without it, tests error with "Invalid Chai property: toBeInTheDocument"
 * because the matcher lives in a separate package.
 */
import "@testing-library/jest-dom/vitest";