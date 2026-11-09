# Explicit import of independent R masks for small, contiguous CyMAE excerpts.
# Never run from CI, or to accommodate a changed Rust result.
suppressPackageStartupMessages({library(flowCore);library(PeacoQC);library(jsonlite)})
stopifnot(as.character(packageVersion("PeacoQC")) == "1.20.0")
args <- commandArgs(TRUE)
registry <- fromJSON(args[1],simplifyVector=FALSE)
stopifnot(isTRUE(registry$complete))
cache <- args[2]; output <- args[3]
dir.create(output,recursive=TRUE,showWarnings=FALSE)
cases <- list()
for (cohort in c("Acute2020","Acute2021","Vaccine")) {
    candidates <- Filter(function(x) startsWith(x$path,paste0("files/",cohort,"/")) && endsWith(x$path,".fcs") && x$bytes >= 4*1024^2,registry$files)
    source <- candidates[[1]]
    ff <- read.FCS(file.path(cache,source$path),transformation=FALSE,truncate_max_range=FALSE,min.limit=-Inf)
    stopifnot(nrow(exprs(ff)) >= 20000)
    ff <- ff[seq_len(20000),]
    target <- file.path(output,paste0(cohort,".fcs"))
    write.FCS(ff,target,what="numeric")
    check <- read.FCS(target,transformation=FALSE,truncate_max_range=FALSE,min.limit=-Inf)
    stopifnot(identical(colnames(ff),colnames(check)),identical(dim(exprs(ff)),dim(exprs(check))),identical(as.vector(exprs(ff)),as.vector(exprs(check))))
    labels <- pData(parameters(ff))$desc
    channels <- colnames(ff)[grepl("CD|IgD|TCR",labels)]
    stopifnot(length(channels) >= 20)
    result <- PeacoQC(ff,channels=channels,determine_good_cells="all",plot=FALSE,save_fcs=FALSE,report=FALSE,output_directory=NULL,MAD=6,IT_limit=0.6,consecutive_bins=5,remove_zeros=FALSE,time_channel_parameter="Time",suppress_message=TRUE)
    reference <- paste0(cohort,".r-mask.json")
    write_json(list(good=as.logical(result$GoodCells)),file.path(output,reference),auto_unbox=TRUE,pretty=FALSE)
    cases[[cohort]] <- list(input=basename(target),reference=reference,source=source,source_rows=list(first=0,last_exclusive=20000),channels=channels)
}
write_json(list(schema_version=1,upstream=list(R=as.character(getRversion()),PeacoQC=as.character(packageVersion("PeacoQC"))),cases=cases),file.path(output,"retention.json"),auto_unbox=TRUE,pretty=TRUE)
