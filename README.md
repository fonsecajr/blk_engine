# BLK — Delta Engine for Massive Game & Asset Pipelines

![BLK Screenshot](./assets/screenshot-main.png)

**BLK** is a high-precision delta snapshot engine designed for massive game folders, mod workflows, and creative pipelines — where traditional Git, rsync, or backup tools fail due to size, noise, and unpredictability.

It creates **versioned layers ("sets")** that capture *only the differences* between states of a directory tree, allowing restorations, experimentation, and clean rollbacks without re-copying entire 50–200 GB installations.

BLK is optimized for use cases like:

- **Assetto Corsa / Mod-heavy games**
- **Flight simulators**
- **Large creative projects (3D, VFX, audio)**
- **Pipelines that need reproducible environments**
- **Developers who want reproducible states without Git-LFS headaches**

It is **fast**, **deterministic**, **portable**, and released under the **MIT License**.

---

# ⚡ Philosophy of BLK

BLK was created from a simple, painful truth:

### > When a game or asset pipeline grows beyond 30–50 GB, no existing tool handles versioning gracefully.

Steam does full downloads.  
CSP and mod managers overwrite files unpredictably.  
Traditional backup tools copy 100 GB just because one file changed.  
Git can’t handle gigantic binary trees without LFS gymnastics.

BLK solves this by:

### ✔ Tracking only *actual* file differences  
### ✔ Saving deltas instead of full snapshots  
### ✔ Restoring states deterministically  
### ✔ Preserving directory “lineage” like a version tree  
### ✔ Providing a clean TUI for humans  
### ✔ Providing a clean engine for developers  

This creates a new mental model:

> “Version the world as it exists on disk — no illusions.”

---

# 🖼 Screenshots

### **Main TUI Interface**
*(example showing lineage, deltas, and active set)*

