# Explicit independent reference for PeacoQC's bounded interior peak selection.
# Preserve the original upstream masks. Never invoked by CI or by a Rust test.
# PeacoQC 1.20.0 indexes 511 x coordinates with 510 logical values, recycling
# selection[1] at the right endpoint. Only this indexing is corrected here.
suppressPackageStartupMessages({library(flowCore);library(PeacoQC);library(jsonlite)})
stopifnot(as.character(packageVersion("PeacoQC")) == "1.20.0")
args <- commandArgs(TRUE)
root <- args[1]
manifest <- fromJSON(file.path(root,"retention.json"),simplifyVector=FALSE)
bounded_peaks <- function(channel_data,remove_zeros,peak_removal=1/3) {
    if (remove_zeros) channel_data <- channel_data[channel_data != 0]
    if (length(channel_data) < 3) return(NA)
    dens <- stats::density(channel_data[!is.na(channel_data)],adjust=1)
    n <- length(dens$y)
    interior <- 2:(n-1)
    selection <- dens$y[interior] > dens$y[interior-1] & dens$y[interior] > dens$y[interior+1] & dens$y[interior] > peak_removal*max(dens$y)
    peaks <- dens$x[interior][selection]
    if (length(peaks) == 0) peaks <- dens$x[which.max(dens$y)]
    peaks
}
assignInNamespace("FindThemPeaks",bounded_peaks,ns="PeacoQC")
reports <- list()
for (name in names(manifest$cases)) {
    case <- manifest$cases[[name]]
    ff <- read.FCS(file.path(root,case$input),transformation=FALSE,truncate_max_range=FALSE,min.limit=-Inf)
    result <- PeacoQC(ff,channels=unlist(case$channels),determine_good_cells="all",plot=FALSE,save_fcs=FALSE,report=FALSE,output_directory=NULL,MAD=6,IT_limit=.6,consecutive_bins=5,remove_zeros=FALSE,time_channel_parameter="Time",suppress_message=TRUE)
    write_json(list(good=as.logical(result$GoodCells)),file.path(root,paste0(name,".r-bounded-mask.json")),auto_unbox=TRUE,pretty=FALSE)
    keywords <- keyword(ff)
    vocabulary <- strsplit(keywords[[which(tolower(names(keywords)) == "omiqfilternames")]],",",fixed=TRUE)[[1]]
    labels <- vocabulary[exprs(ff)[,"OmiqFilter"]+1]
    counts <- table(labels)
    retained <- table(factor(labels[result$GoodCells],levels=names(counts)))
    reports[[name]] <- lapply(names(counts),function(label) list(population=label,original=unname(counts[[label]]),retained=unname(retained[[label]]),frequency_error=unname(retained[[label]]/sum(retained)-counts[[label]]/sum(counts))))
}
write_json(reports,file.path(root,"r-population-retention.json"),auto_unbox=TRUE,pretty=TRUE,digits=17)
