;;; -*- lexical-binding: t -*-

;;; init.el --- Initialization file for Emacs
;;; Commentary: Emacs Startup File --- initialization for Emacs

;; You need to install the JetBrains Mono font for this to work.
;; sudo apt install fonts-jetbrains-mono
;; also good fonts
;; sudo apt install fonts-noto

;; TODO
;; Packages to test:
;; https://www.reddit.com/r/emacs/comments/1m9zshj/update_improved_c_method_stub_generation_with/
f

;;; Code:

;; Straight for package management
(defvar straight-repository-branch "develop")
(defvar straight-use-package-by-default t)
(defvar straight-vc-git-default-clone-depth 1)

;; Bootstrap straight.el
(defvar bootstrap-version)
(let ((bootstrap-file
       (expand-file-name
        "straight/repos/straight.el/bootstrap.el"
        (or (bound-and-true-p straight-base-dir)
            user-emacs-directory)))
      (bootstrap-version 7))
  (unless (file-exists-p bootstrap-file)
    (with-current-buffer
        (url-retrieve-synchronously
         "https://raw.githubusercontent.com/radian-software/straight.el/develop/install.el"
         'silent 'inhibit-cookies)
      (goto-char (point-max))
      (eval-print-last-sexp)))
  (load bootstrap-file nil 'nomessage))

;; This installs use-package if not already installed
(straight-use-package 'use-package)

;; Per-package startup profiling. Must load before any other use-package
;; forms so it can wrap `require'. View results with
;;   M-x benchmark-init/show-durations-tabulated
;; or M-x benchmark-init/show-durations-tree
;; (use-package benchmark-init
;;   :demand t
;;   :config (add-hook 'after-init-hook #'benchmark-init/deactivate))

;; Add guix packages to load-path
(add-to-list 'load-path "~/.guix-profile/share/emacs/site-lisp")

(defun kill-region-or-backward-kill-word (&optional arg region)
  "`kill-region' if the region is active, otherwise `backward-kill-word'."
  (interactive
   (list (prefix-numeric-value current-prefix-arg) (use-region-p)))
  (if region
      (kill-region (region-beginning) (region-end))
    (backward-kill-word arg)))

(defun gcm-scroll-down () (interactive) (scroll-up 4))
(defun gcm-scroll-up   () (interactive) (scroll-down 4))

;; Global keybindings. C-M-s- is the Hyper key on my Ergodox EZ.
(use-package emacs
  :straight nil
  :bind
  (("C-w"            . kill-region-or-backward-kill-word)
   ;; Help is on F1 and helpful further down - free C-h for deletion.
   ("C-h"            . delete-backward-char)
   ("C-c h"          . help-command)
   ("C-c -"          . comment-or-uncomment-region)
   ;; Simpler-to-type M-x.
   ("C-x C-m"        . execute-extended-command)
   ("C-x C-."        . xref-find-definitions)
   ("C-x C-,"        . xref-go-back)
   ;; Hyper-prefixed command set.
   ("C-M-s-c"        . consult-buffer)
   ("C-M-s-i"        . ibuffer)
   ("C-M-s-a"        . backward-paragraph)
   ("C-M-s-e"        . forward-paragraph)
   ("C-M-s-v"        . beginning-of-buffer)
   ("C-M-s-f"        . forward-sexp)
   ("C-M-s-b"        . backward-sexp)
   ("C-M-s-u"        . backward-up-list)
   ("C-M-s-d"        . down-list)
   ("C-M-s-k"        . kill-sexp)
   ("C-M-s-j"        . bookmark-jump)
   ("C-M-s-m"        . bookmark-set)
   ("C-M-s-l"        . bookmark-bmenu-list)
   ("C-M-s-o"        . find-file-other-window)
   ("C-M-s-!"        . shell-command-on-region)
   ("C-M-s-z"        . repeat)
   ("C-M-s-r"        . read-only-mode)
   ;; Scroll without moving point.
   ("C-M-s-p"        . gcm-scroll-up)
   ("C-M-s-n"        . gcm-scroll-down)
   ;; Resize windows.
   ("C-M-s-<left>"   . shrink-window-horizontally)
   ("C-M-s-<right>"  . enlarge-window-horizontally)
   ("C-M-s-<down>"   . shrink-window)
   ("C-M-s-<up>"     . enlarge-window)))


;; Make sure M-tab works for window switching in the desktop
;; I use sway now
;; (global-unset-key (kbd "M-TAB"))
;; Relative line numbers in prog-mode only - global mode redraws the gutter
;; on every cursor move, which is visible in large text/org buffers.
(setq display-line-numbers-type 'relative)
(add-hook 'prog-mode-hook #'display-line-numbers-mode)

(setopt use-short-answers t)

;; Enable transient mark mode
(transient-mark-mode 1)

;; Automatically reread from disk if the underlying file changes
(setopt auto-revert-avoid-polling t)
;; Some systems don't do file notifications well; see
;; https://todo.sr.ht/~ashton314/emacs-bedrock/11
(setopt auto-revert-interval 5)
(setopt auto-revert-check-vc-info t)
(global-auto-revert-mode)

;; Move through windows with Ctrl-<arrow keys>
(windmove-default-keybindings 'control)

;; These were taken from emacs bedrock
(setopt sentence-end-double-space nil)
; Use the minibuffer whilst in the minibuffer
(setopt enable-recursive-minibuffers t)

;; default indentation
(setq-default tab-width 4
              indent-tabs-mode nil)

;; Scrolling
(setq scroll-margin 0
      scroll-conservatively 100000
      scroll-preserve-screen-position nil)

(pixel-scroll-precision-mode 1)

;; Enable repeat mode
(repeat-mode 1)

;; Don't warn about up/downcase-region.
(put 'upcase-region 'disabled nil)
(put 'downcase-region 'disabled nil)

;; Sets environment variable?
(setenv "GPG_AGENT_INFO" nil)
;; Use loopback pinentry for GPG
(setq epa-pinentry-mode 'loopback)

;; TODO: Add code to create these directories if they don't exist
;; Directories
(defvar my/auto-saves-dir
  (expand-file-name "auto-saves/" user-emacs-directory)
  "Directory for auto-save files.")

(defvar my/backups-dir
  (expand-file-name "backups/" user-emacs-directory)
  "Directory for backup files.")

;; Ensure directories exist
(dolist (dir (list my/auto-saves-dir my/backups-dir))
  (unless (file-exists-p dir)
    (make-directory dir t)))

;; Backup settings
(setq backup-by-copying t
      backup-directory-alist `(("." . ,my/backups-dir))  ; or ("." . ,(expand-file-name ...))
      delete-old-versions t
      kept-new-versions 6
      kept-old-versions 2
      version-control t)

;; Auto-save settings
(setq auto-save-file-name-transforms
      `((".*" , my/auto-saves-dir t)))

(use-package project
  :config
  ;; eglot finds project roots via project.el (NOT projectile), and the default VC backend
  ;; returns the git root. Treat dirs with these markers as project roots so the language
  ;; server starts in the crate/module dir rather than an enclosing git root — fixes
  ;; "Failed to discover workspace" for crates nested under e.g. apps/ (the acconneer repo).
  ;; The CLOSEST marker to the file wins. Markers only take effect inside a VC tree.
  (setq project-vc-extra-root-markers '("Cargo.toml"   ;; rust crate
                                        "go.mod"        ;; go module
                                        ".projectile")))

;; Zooming windows, it's like a tiling window manager.
(use-package zoom)

;; Gnus for email. Password needs to be stored in password store with
;; pass edit 127.0.0.1/albin@sm6wjm.se
(use-package gnus
  :straight nil
  :defer t
  :commands (gnus)
  :config
  ;; IMAP via Proton Mail Bridge
  (setq gnus-select-method
        '(nnimap "protonmail"
                 (nnimap-address "127.0.0.1")
                 (nnimap-server-port 1143)
                 (nnimap-stream starttls)
                 (nnimap-user "albin@sm6wjm.se")))

  ;; General Gnus behavior
  (setq gnus-fetch-old-headers 'some
        gnus-use-cache t
        gnus-save-duplicate-list t
        gnus-summary-thread-gathering-function
        'gnus-gather-threads-by-subject)
  ;; Show all folders / labels
  (setq gnus-parameters
        '((".*"
           (display . all)))))

;; Sending mail
(use-package smtpmail
  :straight nil
  :config
  (setq send-mail-function 'smtpmail-send-it
        message-send-mail-function 'smtpmail-send-it
        smtpmail-servers-requiring-authorization ".*"
        user-full-name "Albin Stigö"
        user-mail-address "albin@medurit.se"
        smtpmail-smtp-server "127.0.0.1"
        smtpmail-smtp-service 1025
        smtpmail-stream-type 'starttls
        smtpmail-smtp-user "albin@medurit.se"
        smtpmail-debug-info t))

;; ;; The 0x0 file upload package
;; ;; neat idea but doesn't work very well, maybe I can rewrite it later.
;; (use-package 0x0
;;   :bind (("C-c u" . 0x0-upload-file)
;;          ("C-c C-u" . 0x0-upload-text))
;;   :custom
;;   (0x0-use-curl t))

;; Use emacs for editing browser textareas (atomic chrome)
(use-package atomic-chrome
  :config
  (atomic-chrome-start-server))

;; Bluetooth mode
;; Used for connecting to bluetooth devices
;; Change to https://codeberg.org/rstocker/emacs-bluetooth
(use-package bluetooth
  :straight (:host github :repo "emacsmirror/bluetooth"))

;; guix install emacs-vterm
(use-package vterm
  :straight nil
  :commands (vterm vterm-other-window)
  :bind (("C-c v" . vterm)
         ("C-c V" . vterm-other-window))
  :config
  (setq vterm-max-scrollback 10000))

;; Treesit auto
(use-package treesit-auto
  :custom
  (treesit-auto-install 'prompt)
  :config
  (treesit-auto-add-to-auto-mode-alist 'all)
  (global-treesit-auto-mode))

;; proced
(use-package proced
  :straight (:type built-in)
  :bind ("C-c C-p" . proced)
  :custom
  (proced-auto-update-flag t)
  (proced-goal-attribute nil)
  (proced-show-remote-processes t)
  (proced-enable-color-flag t)
  (proced-format 'custom)
  :config
  (add-to-list
   'proced-format-alist
   '(custom user pid ppid sess tree pcpu pmem rss start time state (args comm))))

;; Enable midnight mode
;; It runs clean-buffer-list at midnight
(use-package midnight
  :config
  (midnight-mode 1))

;; http request library
;; request
(use-package request)

;; helpful, this is very good, take advantage of the hyper key.
(use-package helpful
  :bind (("C-M-s-h f" . helpful-callable)
         ("C-M-s-h F" . helpful-function)
         ("C-M-s-h v" . helpful-variable)
         ("C-M-s-h k" . helpful-key)
         ("C-M-s-h x" . helpful-command)
         ("C-M-s-h ." . helpful-at-point))
  :config
  (setq helpful-max-buffers 10))

;; Find file other file
;; Quickly switch between header and source files
(use-package find-file
  :straight (:type built-in)
  :bind ("C-x C-o" . ff-find-other-file)
  :custom
  (ff-search-directories '("." "../include" "../lib" "../src" "../src/include")))

;; Will use pass for auth-source
(use-package auth-source-pass
  :config  (auth-source-pass-enable))

;; Also install password-store
;; guix install emacs-password-store
(use-package password-store
  ;; builtin
  :straight (:type built-in))

;; Core auth-source configuration
;; this uses password-store
(use-package auth-source
  :straight (:type built-in)
  :after auth-source-pass
  :init
  ;; Only use pass (password-store) for credentials
  (setq auth-sources '(password-store)
        auth-source-save-behavior nil     ; never auto-save new entries
        auth-source-do-cache t            ; enable in-memory caching
        auth-source-cache-expiry 7200))   ; cache for 2 hours (optional)


;; Epg for GPG integration
(use-package epg
  :straight (:type built-in))

;; Highlight current line in programming modes
(use-package hl-line
  :straight (:type built-in)
  :hook ((prog-mode . hl-line-mode)
         (text-mode . hl-line-mode)
         (org-mode . hl-line-mode)))

;; Eldoc shows function signatures in the echo area
(use-package eldoc
  :straight (:type built-in)
  :config
  (setq eldoc-echo-area-use-multiline-p nil))

;; clean up
;; Org mode configuration
(use-package org
  :hook (org-mode . org-indent-mode)
  :bind (("C-c l" . org-store-link)
         ("C-c a" . org-agenda)
         ("C-c c" . org-capture)
         ("C-c j" . org-journal-new-entry))
  :config
  (org-babel-do-load-languages
   'org-babel-load-languages
   '((python . t)
     (shell . t)
     (emacs-lisp . t)
     (C . t)))
  ;; TODO states, the | separates the "active" states from the "done" states
  (setq org-todo-keywords
        '((sequence "TODO" "FEEDBACK" "VERIFY" "|" "DONE" "DELEGATED")))
  ;; Display tweaks that pair with org-modern / org-appear
  (setq org-hide-emphasis-markers t   ; org-appear reveals them on hover
        org-pretty-entities t
        org-ellipsis "…"
        org-auto-align-tags nil
        org-tags-column 0)
  ;; Agendo configuration
  (setq org-directory "~/brain/org"
        org-agenda-include-diary t
        ;; Start the week on Monday and show 3 weeks in the agenda
        org-agenda-start-on-weekday 1
        org-agenda-span 21
		org-agenda-files (list (concat org-directory "/tasks"))
        org-default-notes-file (concat org-directory "/tasks/inbox.org"))
  (setq org-capture-templates
        '(("t" "Todo" entry (file+headline org-default-notes-file "Inbox")
           "* TODO %?\n  %i\n  %a" :prepend t)

          ("j" "Journal" entry (file+datetree (lambda () (expand-file-name "journal.org" org-directory)))
           "* %?\nEntered on %U\n  %i" :empty-lines 1)

          ("m" "Meeting" entry (file+headline org-default-notes-file "Meetings")
           "* MEETING with %? :MEETING:\n  %U\n  - Participants: \n  - Notes: \n  - Action Items: " :clock-in t :clock-resume t)

          ("p" "Project Idea" entry (file+headline org-default-notes-file "Projects")
           "* %?\n  %i\n  %a" :heading-read-only t)

          ("w" "Web Clip" entry (file+headline org-default-notes-file "Web")
           "* %:description\n  Source: %:link\n  Captured on: %U\n  #+BEGIN_QUOTE\n  %i\n  #+END_QUOTE"))))


;; org-protocol enables capture from the browser via emacsclient. Demanded
;; so the protocol handler is registered before any org file is opened.
(use-package org-protocol
  :straight nil
  :demand t)

;; Zettelkasten style note taking
(use-package org-roam
  :custom
  (org-roam-directory (file-truename "~/brain/org/roam"))
  ;; This prevents sync-conflicts and database locking issues.
  (org-roam-db-location "~/.emacs.d/org-roam.db")
  :bind (("C-c n l" . org-roam-buffer-toggle)
         ("C-c n f" . org-roam-node-find)
         ("C-c n g" . org-roam-graph)
         ("C-c n i" . org-roam-node-insert)
         ("C-c n c" . org-roam-capture)
         ;; Dailies
         ("C-c n j" . org-roam-dailies-capture-today))
  :config
  ;; More informative completion interface for vertical UIs (vertico).
  (setq org-roam-node-display-template
        (concat "${title:*} " (propertize "${tags:10}" 'face 'org-tag)))
  (unless (file-exists-p org-roam-directory)
    (make-directory org-roam-directory t))
  (org-roam-db-autosync-mode)
  ;; Show Roam notes in the agenda.
  (add-to-list 'org-agenda-files org-roam-directory))

;; org-roam-protocol enables roam-ref capture from the browser via
;; emacsclient. Demanded so the protocol handler is registered at startup.
(use-package org-roam-protocol
  :straight nil
  :demand t)

;; Typst
(use-package ox-typst
  :after org)

;; Markdown mode
;; sudo apt install pandoc
;; uv tool install grip
(use-package markdown-mode
  :mode (("\\.md\\'" . gfm-mode)           ; GitHub Flavored Markdown for .md
         ("\\.markdown\\'" . markdown-mode))
  :config
  (setq markdown-command "pandoc -f markdown -t html"
        markdown-fontify-code-blocks-natively t ; syntax highlight fenced code blocks
        markdown-header-scaling t               ; scale headers visually
        markdown-hide-urls t))                  ; show link text only, url on hover

;; grip-mode: live browser preview via grip (GitHub-style rendering)
;; uv tool install grip
(use-package grip-mode
  :after markdown-mode
  :bind (:map markdown-mode-map
              ("C-c C-v" . grip-mode)
              :map gfm-mode-map
              ("C-c C-v" . grip-mode)))

;; Mermaid mode
;; npm install -g @mermaid-js/mermaid-cli
;; npm install --prefix ~/.local @mermaid-js/mermaid-cli
(use-package mermaid-mode
  :mode ("\\.mmd\\'" . mermaid-mode)
  :config
  (setq mermaid-mmdc-location "mmdc"  ; assumes mmdc is on PATH
        mermaid-output-format ".svg")
  :bind (:map mermaid-mode-map
         ("C-c C-c" . mermaid-compile)
         ("C-c C-b" . mermaid-compile-buffer)
         ("C-c C-r" . mermaid-compile-region)))

;; ob-mermaid: org-babel support for mermaid diagrams
;; Allows #+begin_src mermaid blocks in org files
(use-package ob-mermaid
  :after org
  :config
  (add-to-list 'org-babel-load-languages '(mermaid . t)))

;; Ox-hugo
(use-package ox-hugo
  :after org)

;; Org-journal. C-c j (bound globally in the org block) creates a new entry;
;; org-journal's own in-buffer prefix is left at the default C-c C-j so it
;; doesn't shadow that binding inside journal buffers.
(use-package org-journal
  :after org
  :config
  (setq org-journal-dir "~/brain/org/journal"
        org-journal-date-format "%A, %d %B %Y"))

;; org-modern - clean, modern org rendering (replaces org-bullets)
(use-package org-modern
  :after org
  :hook ((org-mode . org-modern-mode)
         (org-agenda-finalize . org-modern-agenda)))

;; Fix org-modern block/indent alignment under org-indent-mode (which is
;; enabled via the org :hook). Optional but recommended - drop if undesired.
(use-package org-modern-indent
  :straight (:host github :repo "jdtsmith/org-modern-indent")
  :after org-modern
  :hook (org-mode . org-modern-indent-mode))

;; org-appear - show emphasis markers / links while point is inside them
(use-package org-appear
  :hook (org-mode . org-appear-mode)
  :custom
  (org-appear-autoemphasis t)
  (org-appear-autolinks t)
  (org-appear-autosubmarkers t))

;; org-transclusion - live-include content from other org files/headings
(use-package org-transclusion
  :after org
  :bind (("C-c n t" . org-transclusion-add)
         ("C-c n T" . org-transclusion-mode)))

;; org-download - drag-drop / paste / screenshot images into org buffers.
;; Screenshot uses grim + slurp (Wayland/sway): guix install grim slurp
(use-package org-download
  :after org
  :hook (dired-mode . org-download-enable)
  :custom
  (org-download-method 'directory)
  (org-download-image-dir "images")          ; images/ next to each org file
  (org-download-heading-lvl nil)
  (org-download-screenshot-method "grim -g \"$(slurp)\" %s")
  :bind (:map org-mode-map
              ("C-c n s" . org-download-screenshot)
              ("C-c n y" . org-download-clipboard)))

;; Modern Recent Files integration with Consult
(use-package recentf
  :straight (:type built-in)
  :init
  ;; start recentf early and quietly
  (setq recentf-max-saved-items 500
        recentf-max-menu-items 0            ; don't use old-style menu
        recentf-auto-cleanup 'never)        ; avoid slowdowns with TRAMP or large repos
  :hook (after-init . recentf-mode)
  :config
  ;; Save recentf list periodically
  (run-at-time nil (* 5 60) #'recentf-save-list)) ; every 5 minutes

(use-package corfu
  :custom
  (corfu-auto t)
  (corfu-auto-delay 0.1)
  (corfu-auto-prefix 2)
  (corfu-cycle t)
  (corfu-quit-no-match 'separator)
  :init
  (global-corfu-mode))

;; Corfu in the terminal (if you ever use Emacs -nw)
(use-package corfu-terminal
  :straight (:host codeberg :repo "akib/emacs-corfu-terminal")
  :unless (display-graphic-p)
  :config
  (corfu-terminal-mode +1))

;; Richer annotations in the completion popup (like company-quickhelp)
(use-package nerd-icons-corfu
  :after corfu
  :config
  (add-to-list 'corfu-margin-formatters #'nerd-icons-corfu-formatter))

;; Configure flymake
(use-package flymake
  :hook (emacs-lisp-mode . flymake-mode)
  ;; M-p and M-n to navigate errors
  :bind (:map flymake-mode-map
              ("M-p" . flymake-goto-prev-error)
              ("M-n" . flymake-goto-next-error)))

;; Spell checking with hunspell (English + Swedish)
(use-package flyspell
  :straight (:type built-in)
  :hook ((text-mode . flyspell-mode)
         (prog-mode . flyspell-prog-mode))
  :config
  (setq ispell-program-name "hunspell")
  ;; Use both English and Swedish dictionaries
  (setq ispell-dictionary "en_US,sv_SE")
  (setq ispell-personal-dictionary "~/.hunspell_personal")
  (ispell-set-spellchecker-params)
  (ispell-hunspell-add-multi-dic "en_US,sv_SE"))

;; Vertico for minibuffer completion
;; Without this, there's not completion in the minibuffer.
(use-package vertico
  :init
  (vertico-mode))

;; Orderless completion style
(use-package orderless
  :custom
  ;; Configure a custom style dispatcher (see the Consult wiki)
  ;; (orderless-style-dispatchers '(+orderless-consult-dispatch orderless-affix-dispatch))
  ;; (orderless-component-separator #'orderless-escapable-split-on-space)
  (completion-styles '(orderless basic))
  (completion-category-defaults nil)
  (completion-category-overrides '((file (styles partial-completion)))))

;; Better buffer list
(use-package ibuffer
  :straight (:type built-in)
  :bind ("C-x C-b" . ibuffer))

;; Show whitespace (prog-mode only - global is too noisy in org/magit/etc.)
(use-package whitespace
  :straight (:type built-in)
  :hook (prog-mode . whitespace-mode)
  :config
  (setq whitespace-line-column 80
        whitespace-style '(face empty trailing lines-tail)))

;; Consult for better searching and navigation. It has a lot of
;; features.
(use-package consult
  :bind (("C-s"     . consult-line)
         ("C-c /"   . consult-ripgrep)
         ("C-x b"   . consult-buffer)
         ("M-y"     . consult-yank-pop)
         ("C-c m"   . consult-imenu)
         ("C-x C-r" . consult-recent-file)
         ("C-c M-m" . consult-imenu-multi)))

;; Consult dir
(use-package consult-dir
  :after consult
  :bind (("C-c d" . consult-dir)))

;; Embark makes it easy to choose a command to run based on what is
;; near point, both during a minibuffer completion session.
(use-package embark
  :bind (("C-." . embark-act)         ; act on thing at point
         ("C-," . embark-dwim))
  :init (setq prefix-help-command #'embark-prefix-help-command))

(use-package embark-consult
  :after (embark consult)
  :hook (embark-collect-mode . consult-preview-at-point-mode))

;; Add annotations to the minibuffer completions
(use-package marginalia
  :init (marginalia-mode))

;; Persist history over Emacs restarts. Vertico sorts by history position.
(use-package savehist
  :init
  (savehist-mode))

(use-package tramp
  :init
  (setq tramp-default-method "sshx"
        tramp-verbose 1
        remote-file-name-inhibit-locks t
        remote-file-name-inhibit-auto-save-visited t
        tramp-auto-save-directory (expand-file-name "tramp-autosave" user-emacs-directory)
        tramp-chunksize 8192
        tramp-copy-size-limit (* 2 1024 1024)
        tramp-connection-timeout 10

        ;; Important: defer to ~/.ssh/config for ControlMaster/Persist/Path
        tramp-use-ssh-controlmaster-options nil)
  :config
  ;; async over ssh/sshx
  (connection-local-set-profile-variables
   'remote-direct-async-process
   '((tramp-direct-async-process . t)))
  (dolist (proto '("ssh" "sshx"))
    (connection-local-set-profiles
     `(:application tramp :protocol ,proto)
     'remote-direct-async-process))
  ;; skip VC probes on TRAMP paths
  (setq vc-ignore-dir-regexp
        (format "\\(%s\\)\\|\\(%s\\)" vc-ignore-dir-regexp tramp-file-name-regexp))
  (setq magit-tramp-pipe-stty-settings 'pty))

;; TODO fix this
;; ;; guix install emacs-jinx enchant
;;(use-package jinx
;;   :straight nil
;;   :hook (emacs-startup . global-jinx-mode)
;;   :bind (("M-$" . jinx-correct)
;;          ("C-M-$" . jinx-correct-all)))

;; Highlight "TODO"
(use-package hl-todo
  ;; pull from github
  :straight (:host github :repo "tarsius/hl-todo")
  :hook (prog-mode . hl-todo-mode))

;; Helps you find the right key (built-in in Emacs 30+)
(use-package which-key
  :straight (:type built-in)
  :defer 5
  :init (which-key-mode))

;; Show keypresses on screen (enable manually with keycast-mode)
(use-package keycast)

;; Log commands to a buffer (enable manually with command-log-mode)
(use-package command-log-mode)

;; Move region of text up and down
(use-package move-text
  :bind (("C-S-p" . move-text-up)
         ("C-S-n" . move-text-down)))

;; CMake
(use-package cmake-ts-mode
  :mode "CMakeLists\\.txt\\'"
  :hook ((cmake-ts-mode . cmake-format-on-save-mode)))

;; Smartparens for non-Lisp modes (paredit handles Lisp modes)
(use-package smartparens
  :hook ((emacs-lisp-mode . (lambda () (smartparens-mode -1)))
         (lisp-mode . (lambda () (smartparens-mode -1)))
         (scheme-mode . (lambda () (smartparens-mode -1)))
         (ielm-mode . (lambda () (smartparens-mode -1))))
  :init
  (smartparens-global-mode))

;; Highlight parentheses depending on nesting depth
(use-package rainbow-delimiters
  :hook (prog-mode . rainbow-delimiters-mode))

;; Programming ligatures (composite glyphs like →, ⇒, ≠, ≥).
;; Relies on a ligature-capable font - JetBrains Mono here.
(use-package ligature
  :straight (:host github :repo "mickeynp/ligature.el")
  :hook (prog-mode . ligature-mode)
  :config
  ;; JetBrains Mono ligature set (from upstream README).
  (ligature-set-ligatures
   'prog-mode
   '("-|" "-~" "---" "-<<" "-<" "--" "->" "->>" "-->" "/=" "/=="
     "/\\" "/>" "//" "///" "&&" "||" "||=" "|=" "|>" "^=" "$>" "++"
     "+++" "+>" "=:=" "==" "===" "==>" "=>" "=>>" "<=" "=<<" "=/="
     ">-" ">=" ">=>" ">>" ">>-" ">>=" ">>>" "<*" "<*>" "<|" "<|>"
     "<$" "<$>" "<!--" "<-" "<--" "<->" "<+" "<+>" "<=" "<=="
     "<=>" "<=<" "<>" "<<" "<<-" "<<=" "<<<" "<~" "<~~" "</" "</>"
     "~@" "~-" "~>" "~~" "~~>" "%%" ":<" ":=" "::" ":::" ":>"
     ".." "..." "..<" ".?" "#(" "#_" "#_(" "#?" "#[" "#{" "#:"
     "#!" "#=" "##" "###" "####" ";;" "_|_" "__" "\\\\" "\\\\\\"
     "{|" "[|" "]#" "(*" "}#" "^=" "!!" "!=" "!==" "'''" "\"\"\""
     "***" "*>" "*/")))

;; Paredit for structural editing in Lisp modes
(use-package paredit
  :hook ((emacs-lisp-mode . paredit-mode)
         (lisp-mode . paredit-mode)
         (scheme-mode . paredit-mode)
         (ielm-mode . paredit-mode))
  ;; Make Ctrl-h behave
  :bind (:map paredit-mode-map
              ([remap delete-backward-char] . paredit-backward-delete)))

;; Modus themes
(use-package modus-themes
  :demand t
  :init
  (setq modus-themes-bold-constructs t
        modus-themes-italic-constructs t   ; replaces slanted-constructs
        modus-themes-to-toggle '(modus-operandi-tinted modus-vivendi-tinted))
  :config
  (setq modus-themes-common-palette-overrides
        '((comment yellow-faint)
          (string green-warmer)))
  (modus-themes-load-theme 'modus-operandi-tinted)
  :bind
  ("C-M-s-y" . modus-themes-rotate))

;; Ace
(use-package ace-window
  :bind ("M-o" . ace-window))

;; https://github.com/casouri/vundo
(use-package vundo
  :bind ("C-x u" . vundo)
  :config
  (define-key vundo-mode-map (kbd "C-p") #'vundo-backward)
  (define-key vundo-mode-map (kbd "C-n") #'vundo-forward)
  (define-key vundo-mode-map (kbd "C-b") #'vundo-previous)
  (define-key vundo-mode-map (kbd "C-f") #'vundo-next)
  (define-key vundo-mode-map (kbd "q")   #'vundo-quit))

(use-package projectile
  :init
  (setq projectile-project-search-path nil)
  :bind ("C-x u" . vundo)
  :config
  (setq projectile-cleanup-known-projects t)
  (projectile-mode +1)

  :bind (:map projectile-mode-map
              ("C-c p" . projectile-command-map)))

;; projectile ripgrep
(use-package projectile-ripgrep)

;; Jump around in text quickly.
;; Ctrl-åäl are mapped to M-åäö in alacritty.toml
(use-package avy
  :bind (("C-ö" . avy-goto-char)
         ("M-ö" . avy-goto-char)
         ("C-ä" . avy-goto-char-2)
         ("M-ä" . avy-goto-char-2)
         ("C-å" . avy-goto-line)
         ("M-å" . avy-goto-line)
         ("M-g w" . avy-goto-word-1))
  :config
  (setq avy-all-windows t)
  (setq avy-timeout-seconds 0.3)
  (setq avy-background t))

;; Avy zap, it zaps to a character using avy. It's like zap-to-char but with
;; avy's jumping method.
(use-package avy-zap
  :after avy
  :bind (("M-z" . avy-zap-to-char-dwim)
         ("M-Z" . avy-zap-up-to-char-dwim)))

;; Caddyfile
(use-package caddyfile-mode
  :mode "Caddyfile\\'")

;; Nice cozy fireplace
(use-package fireplace
  :straight (:host github :repo "johanvts/emacs-fireplace"))

;; Erlang mode
(use-package erlang
  :mode (("\\.erl?$" . erlang-mode)
         ("rebar\\.config$" . erlang-mode)
         ("relx\\.config$" . erlang-mode)
         ("sys\\.config\\.src$" . erlang-mode)
         ("sys\\.config$" . erlang-mode)
         ("\\.config\\.src?$" . erlang-mode)
         ("\\.config\\.script?$" . erlang-mode)
         ("\\.hrl?$" . erlang-mode)
         ("\\.app?$" . erlang-mode)
         ("\\.app.src?$" . erlang-mode)
         ("\\Emakefile" . erlang-mode))
  :config
  (setq erlang-indent-level 2))

;; Emacs reformatter
;; Generates code for reformatters
(use-package reformatter
  :config
  ;; These are macros that define reformatters
  ;; clang-format
  (reformatter-define clang-format
    :program "clang-format"
    :args '("--style=llvm" "-"))
  (reformatter-define go-imports
    :program "goimports")
  (reformatter-define blacken
    :program "black"
    :args '("-"))
  (reformatter-define cmake-format
    :program "cmake-format"
    :args '("-"))
  (reformatter-define rustfmt
    :program "rustfmt"
    :args '("--edition" "2024" "--emit" "stdout" "--quiet"))
  ;; prettier reads project .prettierrc; --stdin-filepath sets the parser.
  ;; Install: npm install --prefix ~/.local prettier
  (reformatter-define prettier
    :program "prettier"
    :args `("--stdin-filepath" ,(or (buffer-file-name) "file.tsx"))))

(use-package rust-ts-mode
  :straight (:type built-in)
  ;; make sure reformatter is loaded first, otherwise the modes won't be defined
  :after reformatter
  :hook ((rust-ts-mode . rustfmt-on-save-mode))
  :bind (:map rust-ts-mode-map
              ("C-c C-f" . rustfmt-buffer)))

(use-package c-ts-mode
  :straight (:type built-in)
  :after reformatter
  :hook ((c-ts-mode c++-ts-mode) . clang-format-on-save-mode)
  :bind (:map c-ts-mode-map
              ("C-c C-f" . clang-format-buffer)
         :map c++-ts-mode-map
              ("C-c C-f" . clang-format-buffer)))

;; This require goimports to be installed.
;; go install golang.org/x/tools/gopls@latest
;; go install golang.org/x/tools/cmd/goimports@latest
(use-package go-ts-mode
  :straight (:type built-in)
  :hook ((go-ts-mode . go-imports-on-save-mode)
         (go-ts-mode . electric-pair-mode))
  :config
  ;; https://mail.gnu.org/archive/html/emacs-devel/2023-09/msg01353.html
  (setq go-ts-mode-indent-offset tab-width))

;; TypeScript / TSX (SolidJS). Requires:
;;   npm install --prefix ~/.local typescript typescript-language-server prettier
(use-package typescript-ts-mode
  :straight (:type built-in)
  :after reformatter
  :mode (("\\.ts\\'"  . typescript-ts-mode)
         ("\\.tsx\\'" . tsx-ts-mode)
         ("\\.jsx\\'" . tsx-ts-mode))
  :hook ((typescript-ts-mode tsx-ts-mode) . prettier-on-save-mode)
        ((typescript-ts-mode tsx-ts-mode) . electric-pair-mode)
  :bind (:map typescript-ts-mode-map
              ("C-c C-f" . prettier-buffer)
         :map tsx-ts-mode-map
              ("C-c C-f" . prettier-buffer))
  :config
  (setq typescript-ts-mode-indent-offset 2))

(use-package json-ts-mode
  :straight (:type built-in)
  :mode "\\.json\\'")

(use-package css-ts-mode
  :straight (:type built-in)
  :mode "\\.css\\'")

;; systemd unit file mode
;; there's not treesitter mode for this
(use-package systemd
  :mode "\\.service\\'")

;; Shows the current file path in the header line.
(use-package breadcrumb
  :straight (:host github :repo "joaotavora/breadcrumb")
  :hook (eglot-managed-mode . breadcrumb-local-mode))

;; Use latest eglot from git
(use-package eglot
  :straight (:host github :repo "joaotavora/eglot")
  :hook ((c-ts-mode
          c++-ts-mode
          python-ts-mode
		  ;; rust-mode
          rust-ts-mode
          typescript-ts-mode
          tsx-ts-mode
          go-ts-mode) . eglot-ensure)
  :config
  ;; indentation
  (add-to-list 'eglot-server-programs
               '((c-mode c++-mode c-ts-mode c++-ts-mode)
                 . ("clangd"
                    "--fallback-style=llvm"
                    "--header-insertion=never"
                    ;"--clang-tidy"
                    "--background-index")))
  (add-to-list 'eglot-server-programs
               '((rust-ts-mode) .
                 ("rustup" "run" "stable" "rust-analyzer"
                  :initializationOptions (:check (:command "clippy")))))
  :bind (:map eglot-mode-map
              ;; eglot rename
              ("C-c C-r" . eglot-rename)
              ;; eglot code actions
              ("C-c C-a" . eglot-code-actions)))

;; Eglot integration for consult
(use-package consult-eglot
  :after (consult eglot)
  :bind (:map eglot-mode-map
         ("C-c C-s" . consult-eglot-symbols)))


;; ---------- Git stuff ----------

;; Smerge
;; This is related to resolving conflicts in merges
(use-package smerge-mode
  :init
  (setq smerge-command-prefix (kbd "C-c v")))

;; Magit
(use-package magit)

;; This is for GitHub/GitLab integration
(use-package forge
  :after magit
  :config
  (setq forge-owned-accounts '(("sensrad" . t)
                               ("ast" . t))))

;; Git-gutter, it shows changed lines in the gutter
(use-package git-gutter
  :init
  (global-git-gutter-mode t))

;; Git-timemachine
(use-package git-timemachine
  :bind ("C-c t" . git-timemachine))

;; Git files modes
;; use for .gitignore and .dockerignore
(use-package git-modes
  :mode (("\\.gitignore\\'" . gitignore-mode)
         ("\\.dockerignore\\'" . gitignore-mode)
         ("\\.gitconfig\\'" . gitconfig-mode)))


;; Copilot mode for AI code completion
(use-package copilot
  :straight (:host github :repo "copilot-emacs/copilot.el" :files ("dist" "*.el"))
  :hook ((prog-mode . copilot-mode)
         (yaml-mode . copilot-mode))
  :config
  ;; disable Copilot in special/temp/non-file buffers
  (defun albin/copilot-disable-in-nonfile-or-special ()
    (or (not buffer-file-name)
        (minibufferp)
        (string-prefix-p "*" (buffer-name))
        (derived-mode-p 'special-mode
                        'compilation-mode
                        'comint-mode 'eshell-mode
                        'term-mode 'vterm-mode)))
  (add-to-list 'copilot-disable-predicates #'albin/copilot-disable-in-nonfile-or-special)
  (setq copilot-modeline nil)
  (setq copilot-indent-offset-warning-disable t)

  ;; Let corfu and copilot coexist - tab tries copilot first, falls back to corfu
  (defun albin/copilot-tab-or-corfu ()
    "Accept copilot suggestion if active, otherwise trigger corfu completion."
    (interactive)
    (if (copilot--overlay-visible)
        (copilot-accept-completion)
      (corfu-complete)))

  :bind (("C-c g t" . copilot-mode)
         :map copilot-completion-map
              ("<tab>"   . albin/copilot-tab-or-corfu)
              ("TAB"     . albin/copilot-tab-or-corfu)
              ("C-TAB"   . copilot-accept-completion-by-word)
              ("C-<tab>" . copilot-accept-completion-by-word)))

;; Like godbolt but better
;; Displays assembly output of compiler
(use-package rmsbolt
  :config
  (setq rmsbolt-disassembly-viewer 'disassembly-mode))

;; Swap vertical/horizontal split
(use-package transpose-frame
  :bind ("C-x 4 t" . transpose-frame))

;; Could break authentication
(use-package async
  :config
  (dired-async-mode 1))

;; Add configuration to dired
(use-package dired
  :straight (:type built-in)
  :config
  (setq dired-dwim-target t
        dired-listing-switches "-alh"
        dired-recursive-copies 'always
        dired-recursive-deletes 'always))

;; diredfl add coloring to dired
(use-package diredfl
  :after dired
  :config
  (diredfl-global-mode))

(use-package nerd-icons
  :if (display-graphic-p))

(use-package nerd-icons-dired
  :after nerd-icons
  :hook (dired-mode . nerd-icons-dired-mode))

;; Dockerfile mode
(use-package dockerfile-ts-mode
  :mode "Dockerfile\\'")

;; Cryptography with age
(use-package age
  :straight (:host github :repo "anticomputer/age.el"))

;; Justfile mode
(use-package just-mode)

;; guix
(use-package guix)

(use-package geiser
  :config
  (setq geiser-active-implementations '(guile)))

(use-package geiser-guile
  :after geiser
  :config
  (add-to-list 'geiser-guile-load-path "~/src/guix"))

;; TODO: fix these keybindings.
;; https://github.com/wolray/symbol-overlay
(use-package symbol-overlay
  :hook (prog-mode . symbol-overlay-mode)
  :bind (:map symbol-overlay-map
              ("C-c s n" . symbol-overlay-jump-next)
              ("C-c s p" . symbol-overlay-jump-prev)
              ("C-c s r" . symbol-overlay-rename)
              ("C-c s k" . symbol-overlay-remove-all)))

;; gptel needs transient, it implements keyboard driven menus.
(use-package transient)

;; GPTel for AI chat and code generation
(use-package gptel
  :after transient
    :bind (("C-c g g" . gptel-menu)
           ("C-c g a" . gptel-add)
           ("C-c g r" . gptel-rewrite)
           (:map gptel-mode-map
                 ("C-c C-c" . gptel-send)))
  :after transient
  :config
  ;; Global settings
  (setq gptel-include-reasoning t
        gptel-use-curl t)

  ;; Gemini
  (gptel-make-gemini "Gemini"
    :key (getenv "GOOGLE_GENERATIVE_AI_API_KEY")
    :stream t)

  ;; This is how it sets up the default backend it seems.
  (setq gptel-api-key (getenv "OPENAI_API_KEY"))

  ;; Helper: Auto-scroll the buffer as the AI writes
  (add-hook 'gptel-post-stream-hook 'gptel-auto-scroll))

;; Claude code integration
(use-package claude-code-ide
  :straight (:type git :host github :repo "manzaltu/claude-code-ide.el")
  :bind ("C-c '" . claude-code-ide-menu) ; Set your favorite keybinding
  :config
  (claude-code-ide-emacs-tools-setup)) ; Optionally enable Emacs MCP tools

;; Launch desktop applications from Emacs
(use-package xdg-launcher
  :straight (:host github :repo "emacs-exwm/xdg-launcher" :files ("*.el"))
  :bind (("C-c C-l" . xdg-launcher-run-app)))

;; UUID
(use-package uuidgen)

;; fish shell mode
(use-package fish-mode
  :mode "\\.fish\\'")

(use-package sensrad
  :straight nil
  :load-path "site-lisp")

(use-package albin
  :straight nil
  :load-path "site-lisp")



(provide 'init)
;;; init.el ends here
