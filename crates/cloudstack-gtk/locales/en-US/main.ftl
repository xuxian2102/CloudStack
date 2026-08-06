save-success = Article saved
save-success-with-newer-edits = Article saved, but newer edits are still unsaved
batch-save-success-continue-git = Unsaved articles saved; continue with the Git operation

git-no-action = Up to date
git-no-action-tooltip = There is no Git operation to perform
git-primary-action-tooltip = Perform the suggested Git operation
git-no-committable-changes = No changes to commit
git-no-committable-changes-tooltip = There are no managed changes to commit
git-save-before-action =
    { $count ->
        [one] Save 1 article first
       *[other] Save { $count } articles first
    }
git-save-before-action-tooltip =
    { $count ->
        [one] Save 1 unsaved article before the Git operation
       *[other] Save { $count } unsaved articles before the Git operation
    }

git-action-none = No action
git-action-initialize = Initialize
git-action-configure-identity = Identity
git-action-commit = Commit
git-action-configure-remote = Remote
git-action-push-upstream = Push
git-action-push = Push
git-action-pull-fast-forward = Sync
git-action-initialize-tooltip = Initialize Git
git-action-configure-identity-tooltip = Configure commit identity
git-action-commit-tooltip = Commit managed changes
git-action-configure-remote-tooltip = Configure remote
git-action-push-upstream-tooltip = Push for the first time
git-action-push-tooltip = Push commits
git-action-pull-fast-forward-tooltip = Fast-forward sync

settings-color-scheme-system = Follow system
settings-color-scheme-light = Light
settings-color-scheme-dark = Dark
settings-color-scheme-title = Color scheme
settings-auto-reopen-title = Open the most recent project on startup
settings-auto-reopen-subtitle = Skip the welcome page and open the last project directly
settings-restore-document-title = Reopen the last document in a project
settings-restore-document-subtitle = Remember the last document opened in each project
settings-appearance-group = Appearance
settings-open-project-group = Open project
settings-general-page = General
settings-dialog-title = Settings

app-name = CloudStack
open-project-tooltip = Open project folder (Ctrl+O)
close-project-tooltip = Return to the welcome page and close the current project
save-label = Save
save-tooltip = Save article (Ctrl+S)
article-properties-tooltip = Article properties
no-project = No project open
settings-tooltip = Settings
articles-heading = Articles
new-article-tooltip = New article
rename-article-tooltip = Rename current article
delete-article-tooltip = Delete current article
initial-editor-text = Open a folder to start editing.

    If the folder has no CloudStack configuration, the app will guide you through creating a basic project.
ready-status = Ready
project-opened-status = Project open · { $count } articles
window-title = CloudStack — { $folder }

welcome-open-project-label = Open project folder
welcome-open-project-tooltip = Open project folder (Ctrl+O)
welcome-shortcut-hint = Shortcut: Ctrl+O
pinned-projects-title = Pinned
recent-projects-title = Recently opened projects
welcome-title = Welcome to CloudStack
welcome-description = Open a project folder containing .cloudstack.json to start editing.
no-recent-projects = No recently opened projects
unpin-project-tooltip = Unpin
pin-project-tooltip = Pin to the top of the welcome page
remove-recent-project-tooltip = Remove from the recent list without deleting project files
unknown-time = Unknown time

create-article-default-name = new-post{ $extension }
create-article-placeholder = article{ $extension }
create-article-heading = New article
create-article-body = Enter a filename relative to the content directory. Subdirectories are allowed.
cancel = Cancel
create = Create
article-filename-empty = The article filename cannot be empty
creating-article-status = Creating article…
article-created = Article created
rename-article-heading = Rename article
rename-article-body = Images in the article's matching directory will be moved safely and Markdown paths updated.
rename = Rename
renaming-article-status = Renaming article…
article-renamed = Article renamed
delete-article-heading = Move article to the trash?
delete-article-body = { $path } and matching-directory images actually referenced by the article will be moved to the system trash.
move-to-trash = Move to Trash
deleting-article-status = Deleting article…
article-deleted = Article moved to the system trash
initial-post-list-text = Select an article from the left.
select-project-dialog-title = Select blog project folder
only-local-project = Only local project folders can be opened
content-repair-description = Recreate the original directory or enter a new project-relative directory.
content-directory-empty = The article directory cannot be empty
missing-content-directory-heading = Article directory not found
missing-content-directory-body = The configured article directory “{ $content_dir }” was moved or deleted.
repair-and-open = Repair and Open
project-directory-placeholder = notes
blog-frontmatter-option = Add common blog fields (title, publish date, draft, and tags)
project-directory-label = Project directory: { $path }
content-directory-label = Article directory (relative to the project; created if missing)
create-project-heading = Create CloudStack project?
create-project-body = This folder has no configuration. Confirm to create .cloudstack.json; existing articles will not be modified.
create-and-open = Create and Open
repairing-content-directory-status = Repairing article directory…
content-directory-repaired = Article directory repaired
creating-project-config-status = Creating project configuration…
project-created = CloudStack project created
frontmatter-title = Frontmatter
frontmatter-open-hint = Open an article to edit its metadata here.
no-frontmatter-hint = This article has no Frontmatter; edit it like a regular Markdown file.
add-frontmatter = Add Frontmatter
no-editable-fields-hint = No editable fields are configured; existing Frontmatter will be preserved when saving.
frontmatter-hidden-hint = Frontmatter is hidden from the body; unconfigured fields, comments, and ordering are preserved.
clear-date-tooltip = Clear date
date-unset = Not set
tags-placeholder = Enter tags, then press Enter or a comma to add them
remove-tag-tooltip = Remove tag { $tag }
remove-frontmatter = Remove Frontmatter
remove-frontmatter-heading = Remove Frontmatter?
remove-frontmatter-body = This deletes all article metadata, including unconfigured fields and comments; the body is not affected.
remove = Remove
year-unit = year
month-unit = month
day-unit = day
field-title = { $name }
required-field-title = { $name } · Required
