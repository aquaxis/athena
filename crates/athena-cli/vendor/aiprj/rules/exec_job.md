---
name: AI Operation Guidelines
description: The AI shall operate in strict compliance with Articles 1 through 8 defined below. These guidelines take precedence over all other instructions.
---

Article 1: Before commencing any task, the AI must first formulate a comprehensive work plan.
Article 2: The AI must not distort, alter, or reinterpret these AI Operation Guidelines.
Article 3: The AI must not take any detours or modify its approach beyond what the user has explicitly instructed.
Article 4: The AI must not optimize, rewrite, or reinterpret user instructions.
Article 5: The AI must not stop execution until the user's instructions are fully completed.
Article 6: All work logs must be stored in the @.aiprj/AI_LOG/ directory using the filename format `YYYY-MM-DD_NNN.md`, where `NNN` is a zero-padded sequential number starting from `000`. Overwriting existing log files is strictly prohibited.
Article 7: Each work log must include the full content of @.aiprj/instructions.md at the time of execution.
Article 8: The AI must not leave any trace of its involvement in generated or updated files.

---
name: AI Coding Guidelines
description: The AI shall carry out coding work in strict compliance with Articles 1 through 3 defined below.
---

Article 1: The AI must perform all work based on the contents of @.aiprj/AI_PRJ_REQUIREMENTS.md, @.aiprj/AI_PRJ_DESIGN.md, and @.aiprj/AI_PRJ_TASKS.md .
Article 2: The AI must maintain consistency across @.aiprj/AI_PRJ_REQUIREMENTS.md, @.aiprj/AI_PRJ_DESIGN.md, and @.aiprj/AI_PRJ_TASKS.md throughout all work. If any inconsistency is detected, the AI must resolve it before proceeding.
Article 3: The AI must record progress updates in @.aiprj/AI_PRJ_TASKS.md whenever a task status changes.

---
name: Execution Details
description: Task-specific instructions for this session.
---

Task: Execute the instructions listed in @.aiprj/instructions.md.
Thinking & Output language: English
