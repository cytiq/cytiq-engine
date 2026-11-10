#!/usr/bin/env Rscript
# Black-box comparison with pinned CytoNorm. Does not read or translate its code.
suppressPackageStartupMessages({library(CytoNorm);library(flowCore);library(jsonlite);library(mclust)})
stopifnot(as.character(packageVersion("CytoNorm"))=="2.0.12")
binary <- normalizePath(if(length(commandArgs(TRUE))) commandArgs(TRUE)[1] else "target/debug/cytiq")
work <- normalizePath("parity/.env",mustWork=TRUE)
out <- file.path(work,"cytonorm-parity")
dir.create(out,recursive=TRUE,showWarnings=FALSE)
run <- function(args) {status<-system2(binary,shQuote(args));if(status!=0)stop("cytiq failed: ",paste(args,collapse=" "))}
write_frame <- function(values,path,processed=FALSE) {
  ff <- flowFrame(values)
  p <- Biobase::pData(parameters(ff));p$desc<-unname(c(A="CD3",B="CD4",C="CD8",Time="Time",Original_ID="Original_ID")[colnames(values)])
  parameters(ff)<-Biobase::AnnotatedDataFrame(p)
  keyword(ff)[["$CYT"]]<-"synthetic"
  if(processed){
    keyword(ff)[["CYTIQ_COMPENSATION"]]<-toJSON(list(source="fcs",channels=c("A","B","C")),auto_unbox=TRUE)
    keyword(ff)[["CYTIQ_TRANSFORM"]]<-toJSON(list(method="arcsinh",parameters=list(cofactor=150),files=list(list(channels=c("A","B","C")))),auto_unbox=TRUE)
  }
  write.FCS(ff,path,what="numeric",endian="little")
}
read_values <- function(path) exprs(read.FCS(path,transformation=FALSE,truncate_max_range=FALSE))
make_sample <- function(shift,bio,phase){
  i<-0:2999
  # Three populations separated by two independent backbone markers.
  cbind(A=rep(c(0,10,0),each=1000)+(i%%1000)/1000+shift+phase,
        C=rep(c(0,0,10),each=1000)+((37*i)%%1000)/1000+shift+phase,
        B=2+(i%%1000)/1000+shift+bio+phase,Time=i,Original_ID=i+1)
}
files<-setNames(file.path(out,paste0(c("a","b","va","vb"),".fcs")),c("a","b","va","vb"))
for(name in names(files))write_frame(make_sample(if(name%in%c("b","vb"))1 else 0,if(startsWith(name,"v"))3 else 0,if(startsWith(name,"v"))0.001 else 0),files[[name]],TRUE)
manifest<-list(schema_version=1,panel=list(id="P1",markers=list(list(marker="CD3",channel="A"),list(marker="CD8",channel="C"),list(marker="CD4",channel="B"))),
  mode="controls",clustering_markers=c("CD3","CD8"),normalization_markers=c("CD3","CD4"),som=list(xdim=6,ydim=6,metaclusters=3),training_events=6000,
  samples=lapply(names(files),function(id)list(id=id,batch=if(id%in%c("b","vb"))"b" else "a",file=files[[id]],role=if(startsWith(id,"v"))"validation" else "train")))
manifest_path<-file.path(out,"batches.json");write_json(manifest,manifest_path,auto_unbox=TRUE,pretty=TRUE)
model_dir<-file.path(out,"model")
run(c("cytonorm","train","--manifest",manifest_path,"--output",model_dir,"--overwrite"))
som<-fromJSON(file.path(model_dir,"som.json"))
labels<-list();normalized<-list()
for(id in names(files)){
  csv<-file.path(out,paste0(id,".csv"))
  run(c("flowsom","apply","--input",files[[id]],"--model",file.path(model_dir,"som.json"),"--output",csv,"--overwrite"))
  labels[[id]]<-read.csv(csv)$flowsom_metacluster
  target<-file.path(out,paste0(id,".norm.fcs"))
  run(c("cytonorm","apply","--input",files[[id]],"--model",model_dir,"--batch",if(id%in%c("b","vb"))"b" else "a","--output",target,"--overwrite"))
  normalized[[id]]<-read_values(target)
}
# Hold clustering assignments constant to isolate quantile and spline numerics.
conditional_error<-0
for(cluster in 1:3){
  training<-setNames(file.path(out,paste0(c("a","b"),"-cluster-",cluster,".fcs")),c("a","b"))
  for(id in names(training))write_frame(read_values(files[[id]])[labels[[id]]==cluster,c("A","B"),drop=FALSE],training[[id]])
  reference<-QuantileNorm.train(files=unname(training),labels=names(training),channels=c("A","B"),transformList=NULL,
    nQ=99,quantileValues=seq(.01,.99,.01),goal="mean")
  for(id in names(files)){
    batch<-if(id%in%c("b","vb"))"b" else "a";rows<-which(labels[[id]]==cluster)
    original<-read_values(files[[id]])
    for(marker in c("A","B")){
      expected<-reference$splines[[batch]][[marker]](original[rows,marker])
      conditional_error<-max(conditional_error,max(abs(expected-normalized[[id]][rows,marker])))
    }
  }
}
stopifnot(conditional_error<=1e-9)
# Independent full upstream clustering, with its own RNG: compare populations,
# rather than requiring identical node numbers or individual floating values.
upstream<-CytoNorm.train(files=unname(files[c("a","b")]),labels=c("a","b"),channels=c("A","B"),transformList=NULL,
  FlowSOM.params=list(nCells=6000,xdim=6,ydim=6,nClus=3,scale=FALSE,colsToUse=c("A","C")),
  normParams=list(nQ=99,quantileValues=seq(.01,.99,.01)),outputDir=file.path(out,"r-model"),seed=1,plot=FALSE,recompute=TRUE,
  transformation=FALSE,truncate_max_range=FALSE)
stopifnot(!is.null(upstream$fsom))
ari<-c()
for(id in names(files)){
  fsom<-FlowSOM::NewData(upstream$fsom,files[[id]],transform=FALSE,scale=FALSE)
  rlabels<-FlowSOM::GetMetaclusters(fsom)
  ari<-c(ari,adjustedRandIndex(labels[[id]],rlabels))
}
stopifnot(min(ari)>=0.95)
biology_retained<-mean(normalized$vb[,"B"]-normalized$b[,"B"])
stopifnot(biology_retained>=2.7)
metrics<-list(CytoNorm=as.character(packageVersion("CytoNorm")),conditional_max_absolute_error=conditional_error,
             full_pipeline_min_ARI=min(ari),biological_shift_retained=biology_retained)
results <- Sys.getenv("CYTIQ_PARITY_RESULTS", unset="target/scientific/results")
dir.create(results, recursive=TRUE, showWarnings=FALSE)
write_json(metrics,file.path(results,"metrics_cytonorm.json"),auto_unbox=TRUE,pretty=TRUE,digits=17)
print(metrics)
