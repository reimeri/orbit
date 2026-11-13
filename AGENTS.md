# General implementation guidance

Project-specific requirements override these defaults.

Use `nix develop` environment for development tooling.

## Project
- Start by reading the PROJECT.md file for information about this project and the goals

## Priorities and scope
- Identify the primary user task and prioritize it over supporting features.
- Keep implementation simple without reducing useful functionality to a barebones demo.

## Failure recovery
- When something inevitably goes wrong the last thing you want to do is to just present the user a error message and let them figure out how to continue
- Think of this like the user is the customer. When our service fails for some reason we don't just abandon the user and let them sort it out, no we try to everything we can to make our customer happy
- Always try to automatically recover from failures and if that is not possible provide a friendly error message to the user with succinct steps to fix the issue

## Backwards compatability
- No backwards compatability should be maintained in this project.
