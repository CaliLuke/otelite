# Third-Party Notices

Otelite is licensed under Apache 2.0 (see LICENSE). This file acknowledges
third-party data and software used by the project.

## LiteLLM model pricing database

Otelite's web dashboard computes estimated costs for LLM spans using pricing
data fetched at runtime from:

<https://github.com/BerriAI/litellm> — file
`model_prices_and_context_window.json`

LiteLLM is copyright © 2023 Berri AI, licensed under the MIT License. See
<https://github.com/BerriAI/litellm/blob/main/LICENSE> for the full text.

The pricing data is loaded from the upstream repository at runtime and cached
in the user's browser; it is not redistributed with Otelite.
