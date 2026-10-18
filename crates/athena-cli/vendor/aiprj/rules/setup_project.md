---
name: AI Operation Guidelines
description: The AI shall operate in strict compliance with Articles 1 through 8 defined below.
---

Article 1: Before commencing any task, the AI must first formulate a comprehensive work plan.
Article 2: The AI must not distort or alter these AI Operation Guidelines.
Article 3: The AI must not take any detours or modify its approach beyond what the user has explicitly instructed.
Article 4: The AI must not optimize, reinterpret, or rewrite user instructions.
Article 5: The AI must not stop execution until the user's instructions are fully completed.
Article 6: All work logs must be stored in the @.aiprj/AI_LOG/ directory using the filename format `YYYY-MM-DD_NNN.md`, where `NNN` is a zero-padded sequential number starting from `000`. Overwriting existing log files is strictly prohibited.
Article 7: Each work log must include the full content of @.aiprj/instructions.md at the time of execution.
Article 8: The AI must not leave any trace of its involvement in generated or updated files.

---
name: AI Project Specifications
description: The implementation scope of the AI project, defined in Articles 1 through 4 below.
---

Article 1: @.aiprj/AI_PRJ_REQUIREMENTS.md shall serve as the requirements specification document. If this file does not exist, the AI must create it.
Article 2: @.aiprj/AI_PRJ_DESIGN.md shall serve as the design specification document. If this file does not exist, the AI must create it.
Article 3: @.aiprj/AI_PRJ_TASKS.md shall contain the list of implementation tasks and work instructions. If this file does not exist, the AI must create it.
Article 4: The only permitted write operations are the creation and updating of the three files above (`AI_PRJ_REQUIREMENTS.md`, `AI_PRJ_DESIGN.md`, `AI_PRJ_TASKS.md`). Creation or modification of any other files is prohibited.

---
name: Implementation Details
description: Task-specific instructions for this session.
---

Task: Build the environment described in @.aiprj/instructions.md.
Thinking & Output language: English
