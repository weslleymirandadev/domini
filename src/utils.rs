pub fn print_help() {
    println!(
        r#"
Available commands:
  help                      Show this help message
  list                      List connected agents
  use <index>               Select an agent to interact with
  interact                  Enter interactive mode with selected agent
  send <command>            Send command to selected agent (wait for response)
  send-async <command>      Send command to selected agent (no wait)
  broadcast <command>       Send command to all agents (wait for responses)
  broadcast-async <command> Send command to all agents (no wait)
  clear                     Clear screen
  exit / quit               Exit control panel
"#
    );
}

pub fn print_banner() {
    println!(
        r#"
                                                          
          @                                               
         @@@@ @                                           
        %@@@@@@@%                                         
         @@@@@@@@@@@                                      
       %%%%@@@@@@@@@@%%%%                                 
        %%%@%%@@@@@@@@@%%%%%        # ##****%             
         %@%%%@@%@@@@@@@@%%%%%  %%%#####%##%#             
           %%%@@@%@@@@@@@@@@@%#%###%%@%%%%%%#*            
            %%%%%%%%%%@@@@@@@@@%#%%@@%%%@@%%@%#           
              %%%%%%%%%@@@@@@@@%%%@%%%%%%@@@@@@#          
              %#%%%%#%%%%%%%%%%%%%%%##%%%@   @@@          
                  *****####%%%%%%%%%##*#%%                
                    *+++***##%%%%%%%##***#                
                   ++++=++*%%%%%%%%%%%%%%%@               
              *****###%###%%%@@@@%%@%###@@@               
             %%@%%%%%%%%%%%%@%@@@@@@@%@@@@%%@@@%%         
            %@@@%@@@%%%@@@@@@%@@@@@@@@@@@@@@@@@@@@        
           %@@@@@@@@@%@@@@@@@@@@@%%          @@ @@        
         %%@@@@% @@@@@@@@@                  @@  @@        
        %%@@@%%@@@@@@@@@@               @@@@@@ @@@        
       %%@@@@@@@@@@@@@%                       @@@         
      %%@@%%@@   @@@@                                     
    %%@@%%%@@   @@@@                                      
   @%%%%%%%@     @@@                                      
   @%@ @%%@       @%@                                     
       %%@         %%%                                    
                     %%%                                  
                                                          
                                                          
                ██▄   ████▄ █▀▄▀█ ▄█    ▄  ▄█ 
                █  █  █   █ █ █ █ ██     █ ██ 
                █   █ █   █ █ ▄ █ ██ ██  █ ██ 
                █  █  ▀████ █   █ ▐█ █ █ █ ▐█ 
                ███▀           █   ▐ █  ██  ▐ 
                              ▀      █  ██    
                              
"#
    );
}