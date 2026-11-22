# Research arm of the CytoScan novelty benchmark (`novelty_benchmark.py
# --with-r-prior`): mclust's default model search with its conjugate prior
# (`priorControl()`, Fraley & Raftery 2007) on production reference features,
# thresholded in sample at the 5% quantile as CytoScan does. Never a
# production candidate in rule v1.
#
#   Rscript novelty_prior.R DIR   # DIR/reference.csv, DIR/test.csv (no header)
args <- commandArgs(trailingOnly = TRUE)
directory <- args[[1]]
suppressPackageStartupMessages(library(mclust))
reference <- as.matrix(read.csv(file.path(directory, "reference.csv"), header = FALSE))
test <- as.matrix(read.csv(file.path(directory, "test.csv"), header = FALSE))
fit <- Mclust(reference, prior = priorControl(), verbose = FALSE)
if (is.null(fit)) stop("Mclust with priorControl() fitted no model")
reference_density <- dens(reference, modelName = fit$modelName, parameters = fit$parameters, logarithm = TRUE)
test_density <- dens(test, modelName = fit$modelName, parameters = fit$parameters, logarithm = TRUE)
threshold <- unname(quantile(reference_density, 0.05, type = 7))
write.table(matrix(test_density, ncol = 1), file.path(directory, "test_density.csv"),
            row.names = FALSE, col.names = FALSE, sep = ",")
writeLines(format(threshold, digits = 17), file.path(directory, "threshold.txt"))
