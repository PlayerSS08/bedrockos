# OS CAs – Operating Systems (25PCCS201), research topic R19

One folder per CA; **each folder is a self-contained project** that can be run on its own.
CA-6 (Rust micro-OS) is BedrockOS itself — the rest of this repository.

| Folder | CA | Status |
|---|---|---|
| [`CA1_Systems_Thinking`](CA1_Systems_Thinking/) | CA-1 | Systems Thinking Challenge: F1 high page fault rate (report A–K, stdlib code, 9 tests) |
| [`CA2_Performance_Investigation`](CA2_Performance_Investigation/) | CA-2 | complete: code, results, 23-page report |
| [`CA3_Design_and_Debugging`](CA3_Design_and_Debugging/) | CA-3 | complete: code, results, report |
| [`CA4_Research_Mini_Project`](CA4_Research_Mini_Project/) | CA-4 | research report (PDF) |
| [`CA5_Linux_Systems_Engineering`](CA5_Linux_Systems_Engineering/) | CA-5 | complete: code, results, report (Linux needed to re-run) |

CA-2, CA-3 and CA-5 each contain their own copy of the small shared simulator (`src/`), so any one of them can be run alone.
Python 3.10+ is enough (`pip install -r requirements.txt`); building the .docx/.pdf additionally needs pandoc and LibreOffice.
