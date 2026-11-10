# Export the published CATALYST PBMC example without transforming expression.
# Downloaded R data are pinned by source commit and SHA-256 in normal.lock.json.
suppressPackageStartupMessages({ library(flowCore); library(jsonlite) })
args <- commandArgs(TRUE)
source <- args[1]; output <- args[2]
dir.create(output, recursive=TRUE, showWarnings=FALSE)
for (name in c("PBMC_fs", "PBMC_md", "PBMC_panel")) load(file.path(source, paste0(name,".rda")))
write.csv(PBMC_md, file.path(output,"subjects.csv"), row.names=FALSE)
write.csv(PBMC_panel, file.path(output,"panel.csv"), row.names=FALSE)
stopifnot(length(PBMC_fs) == nrow(PBMC_md))
records <- list()
for (i in seq_along(PBMC_fs)) {
    frame <- PBMC_fs[[i]]
    path <- file.path(output,paste0("sample_",i,".fcs"))
    write.FCS(frame,path,what="numeric")
    # Export precision is measured, not assumed; source values remain in R data.
    reread <- read.FCS(path,transformation=FALSE,truncate_max_range=FALSE)
    stopifnot(identical(dim(exprs(frame)),dim(exprs(reread))))
    delta <- max(abs(exprs(frame)-exprs(reread))/pmax(1,abs(exprs(frame))))
    stopifnot(is.finite(delta),delta < 1e-6)
    records[[i]] <- list(path=basename(path),source_row=i,metadata=as.list(PBMC_md[i,,drop=FALSE]),max_scaled_export_error=delta)
}
write_json(list(files=records),file.path(output,"export.json"),pretty=TRUE,auto_unbox=TRUE,digits=17)
