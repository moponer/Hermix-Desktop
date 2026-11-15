// Make @testing-library/jest-dom's vitest matchers (toBeInTheDocument,
// toHaveTextContent, …) visible to `tsc --noEmit`. The runtime wiring
// lives in `vitest.setup.ts`; this file ensures the type augmentation
// is part of the compilation program so component tests type-check
// without "Property 'toBeInTheDocument' does not exist" errors.
import "@testing-library/jest-dom/vitest";
